//! Design and acceptance: frontend tsz/docs/features/wordlist-folders/design.md.
pub mod dto;
mod handler;
use super::{
    deletion::{self, DeletionActor},
    dto::{Wordlist, WordlistState},
    service::{self, Tx},
};
use crate::{
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
};
use chrono::{DateTime, Utc};
use dto::*;
pub use handler::*;
use serde::Serialize;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Clone, Serialize, sqlx::FromRow)]
struct Node {
    id: Uuid,
    parent_id: Option<Uuid>,
    wordlist_id: Option<Uuid>,
    name: String,
    trashed_at: Option<DateTime<Utc>>,
    trash_root_id: Option<Uuid>,
    revision: i64,
}
fn invalid(message: &str) -> AppError {
    AppError::bad_request(ErrorCode::ValidationFailed, message)
}
fn missing() -> AppError {
    AppError::not_found("目录项目不存在")
}
fn conflict(message: &str) -> AppError {
    AppError::conflict(ErrorCode::WordListConflict, None, message)
}
fn name(value: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 100 {
        return Err(invalid("名称须为 1–100 字"));
    }
    Ok(value.to_owned())
}
pub async fn lock(tx: &mut Tx<'_>, user: Uuid) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("wordlist-library:{user}"))
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    Ok(())
}
async fn begin(pool: &PgPool, auth: &AuthUser) -> Result<Tx<'static>, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::lock_user(&mut tx, auth).await?;
    lock(&mut tx, auth.subject).await?;
    Ok(tx)
}
async fn commit(mut tx: Tx<'_>, auth: &AuthUser) -> Result<(), AppError> {
    // A library or wordlist lock may outlive the account's deletion grace period.
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)
}
async fn load(tx: &mut Tx<'_>, user: Uuid, space: LibrarySpace) -> Result<Vec<Node>, AppError> {
    sqlx::query_as("SELECT id,parent_id,wordlist_id,name,trashed_at,trash_root_id,revision FROM wordlist_library_nodes WHERE user_id=$1 AND space=$2 ORDER BY id")
        .bind(user).bind(space.as_str()).fetch_all(&mut **tx).await.map_err(AppError::internal)
}
fn version(nodes: &[Node]) -> Result<String, AppError> {
    Ok(service::hash(&nodes)?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn check_version(nodes: &[Node], expected: &str) -> Result<(), AppError> {
    if version(nodes)? != expected {
        return Err(conflict("目录已改变，请刷新后重新确认"));
    }
    Ok(())
}
fn find(nodes: &[Node], id: Uuid) -> Result<&Node, AppError> {
    nodes.iter().find(|n| n.id == id).ok_or_else(missing)
}
fn target(nodes: &[Node], parent: Option<Uuid>) -> Result<(), AppError> {
    if let Some(id) = parent {
        let n = find(nodes, id)?;
        if n.wordlist_id.is_some() || n.trashed_at.is_some() {
            return Err(invalid("目标必须是正常文件夹"));
        }
    }
    Ok(())
}
fn path(nodes: &[Node], mut parent: Option<Uuid>) -> Result<Vec<LibraryCrumb>, AppError> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    while let Some(id) = parent {
        if !seen.insert(id) {
            return Err(invalid("不能将文件夹移动到自身或子文件夹"));
        }
        let n = find(nodes, id)?;
        result.push(LibraryCrumb {
            id,
            name: n.name.clone(),
        });
        parent = n.parent_id;
    }
    result.reverse();
    Ok(result)
}
fn visible_path(
    nodes: &[Node],
    parent: Option<Uuid>,
    trash_root: Option<Uuid>,
) -> Result<Vec<LibraryCrumb>, AppError> {
    let mut crumbs = path(nodes, parent)?;
    if let Some(root) = trash_root {
        crumbs.retain(|p| find(nodes, p.id).is_ok_and(|n| n.trash_root_id == Some(root)));
    }
    Ok(crumbs)
}
fn validate_tree(nodes: &[Node]) -> Result<(), AppError> {
    let mut names = HashSet::new();
    for n in nodes.iter().filter(|n| n.trashed_at.is_none()) {
        target(nodes, n.parent_id)?;
        let depth = path(nodes, n.parent_id)?.len() + usize::from(n.wordlist_id.is_none());
        if depth > 20 {
            return Err(invalid("文件夹最多 20 层"));
        }
        if n.wordlist_id.is_none() && !names.insert((n.parent_id, n.name.clone())) {
            return Err(conflict("目标目录已有同名文件夹"));
        }
    }
    Ok(())
}
async fn save(tx: &mut Tx<'_>, n: &Node) -> Result<(), AppError> {
    sqlx::query("UPDATE wordlist_library_nodes SET parent_id=$2,name=$3,trashed_at=$4,trash_root_id=$5,revision=revision+1 WHERE id=$1")
        .bind(n.id).bind(n.parent_id).bind(&n.name).bind(n.trashed_at).bind(n.trash_root_id).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
// Called only after the account and library lock, inside the existing creation transaction.
pub async fn add_wordlist(
    tx: &mut Tx<'_>,
    user: Uuid,
    space: LibrarySpace,
    wordlist: Uuid,
    parent: Option<Uuid>,
) -> Result<(), AppError> {
    let nodes = load(tx, user, space).await?;
    if let Some(existing) = nodes.iter().find(|n| n.wordlist_id == Some(wordlist)) {
        if existing.trashed_at.is_some() {
            let mut n = existing.clone();
            n.parent_id = parent;
            n.trashed_at = None;
            n.trash_root_id = None;
            target(&nodes, parent)?;
            save(tx, &n).await?;
        }
        return Ok(());
    }
    target(&nodes, parent)?;
    sqlx::query("INSERT INTO wordlist_library_nodes(id,user_id,space,parent_id,wordlist_id) VALUES($1,$2,$3,$4,$5)")
        .bind(Uuid::now_v7()).bind(user).bind(space.as_str()).bind(parent).bind(wordlist).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
pub async fn remove_favorite(tx: &mut Tx<'_>, user: Uuid, wordlist: Uuid) -> Result<(), AppError> {
    sqlx::query("UPDATE wordlist_library_nodes SET trashed_at=clock_timestamp(),trash_root_id=id,revision=revision+1 WHERE user_id=$1 AND space='favorites' AND wordlist_id=$2 AND trashed_at IS NULL")
        .bind(user).bind(wordlist).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
pub async fn listing(
    pool: &PgPool,
    auth: &AuthUser,
    space: LibrarySpace,
    query: LibraryQuery,
) -> Result<LibraryPage, AppError> {
    let mut tx = begin(pool, auth).await?;
    let nodes = load(&mut tx, auth.subject, space).await?;
    let trash = query.trash.unwrap_or(false);
    let folders_only = query.folders_only.unwrap_or(false);
    let q = query.q.as_deref().unwrap_or("").trim().to_lowercase();
    let page = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(50);
    if page == 0 || !(1..=100).contains(&size) || q.chars().count() > 100 {
        return Err(invalid("分页或搜索参数无效"));
    }
    let ancestors = if let Some(id) = query.parent_id {
        let n = find(&nodes, id)?;
        if n.wordlist_id.is_some() || n.trashed_at.is_some() != trash {
            return Err(missing());
        }
        visible_path(&nodes, Some(id), n.trash_root_id)?
    } else {
        vec![]
    };
    let ids: Vec<_> = nodes.iter().filter_map(|n| n.wordlist_id).collect();
    let lists:Vec<Wordlist>=sqlx::query_as(sqlx::AssertSqlSafe(format!("{} WHERE w.id=ANY($1) AND (( $2='owned' AND w.owner_user_id=$3 AND (w.deleted_at IS NULL OR EXISTS(SELECT 1 FROM wordlist_library_nodes n WHERE n.user_id=$3 AND n.space='owned' AND n.wordlist_id=w.id AND n.trashed_at IS NOT NULL))) OR ($2='favorites' AND w.deleted_at IS NULL AND w.state='published' AND {}))",service::META,service::VISIBLE_OWNER)))
        .bind(ids).bind(space.as_str()).bind(auth.subject).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let mut lists: HashMap<_, _> = lists.into_iter().map(|w| (w.id, w)).collect();
    let mut items = Vec::new();
    for n in &nodes {
        if n.trashed_at.is_some() != trash || (folders_only && n.wordlist_id.is_some()) {
            continue;
        }
        if query.wordlist_id.is_some() && n.wordlist_id != query.wordlist_id {
            continue;
        }
        if !folders_only && q.is_empty() && query.wordlist_id.is_none() {
            if let Some(parent) = query.parent_id {
                if n.parent_id != Some(parent)
                    || (trash && n.trash_root_id != find(&nodes, parent)?.trash_root_id)
                {
                    continue;
                }
            } else if trash {
                if n.trash_root_id != Some(n.id) {
                    continue;
                }
            } else if n.parent_id.is_some() {
                continue;
            }
        }
        let wordlist = n.wordlist_id.and_then(|id| lists.remove(&id));
        let label = if !n.name.is_empty() {
            n.name.clone()
        } else {
            wordlist
                .as_ref()
                .map(|w| w.name.clone())
                .unwrap_or_else(|| "原词表不可用".into())
        };
        if !q.is_empty()
            && !label.to_lowercase().contains(&q)
            && !wordlist
                .as_ref()
                .is_some_and(|w| w.name.to_lowercase().contains(&q))
        {
            continue;
        }
        items.push(LibraryItem {
            id: n.id,
            parent_id: n.parent_id,
            wordlist_id: n.wordlist_id,
            name: label,
            revision: n.revision,
            trashed_at: n.trashed_at,
            path: visible_path(&nodes, n.parent_id, n.trash_root_id)?,
            wordlist,
        });
    }
    items.sort_by(|a, b| {
        a.wordlist_id
            .is_some()
            .cmp(&b.wordlist_id.is_some())
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    let total = items.len() as i64;
    let items = items
        .into_iter()
        .skip((u64::from(page - 1) * u64::from(size)) as usize)
        .take(size as usize)
        .collect();
    let result = LibraryPage {
        version: version(&nodes)?,
        ancestors,
        items,
        pagination: service::page_meta(page, size, total),
    };
    commit(tx, auth).await?;
    Ok(result)
}
pub async fn create_folder(
    pool: &PgPool,
    auth: &AuthUser,
    space: LibrarySpace,
    input: LibraryFolderInput,
) -> Result<(), AppError> {
    let mut tx = begin(pool, auth).await?;
    let mut nodes = load(&mut tx, auth.subject, space).await?;
    check_version(&nodes, &input.expected_version)?;
    target(&nodes, input.parent_id)?;
    let n = Node {
        id: Uuid::now_v7(),
        parent_id: input.parent_id,
        wordlist_id: None,
        name: name(&input.name)?,
        trashed_at: None,
        trash_root_id: None,
        revision: 1,
    };
    nodes.push(n.clone());
    validate_tree(&nodes)?;
    sqlx::query("INSERT INTO wordlist_library_nodes(id,user_id,space,parent_id,name) VALUES($1,$2,$3,$4,$5)").bind(n.id).bind(auth.subject).bind(space.as_str()).bind(n.parent_id).bind(n.name).execute(&mut *tx).await.map_err(AppError::internal)?;
    commit(tx, auth).await
}
pub async fn rename_node(
    pool: &PgPool,
    auth: &AuthUser,
    space: LibrarySpace,
    id: Uuid,
    input: LibraryRename,
) -> Result<(), AppError> {
    let mut tx = begin(pool, auth).await?;
    let mut nodes = load(&mut tx, auth.subject, space).await?;
    check_version(&nodes, &input.expected_version)?;
    let index = nodes.iter().position(|n| n.id == id).ok_or_else(missing)?;
    if nodes[index].trashed_at.is_some() {
        return Err(missing());
    }
    let value = name(&input.name)?;
    if space == LibrarySpace::Owned
        && let Some(w) = nodes[index].wordlist_id
    {
        let current = service::lock_list(&mut tx, w, Some(auth.subject), true).await?;
        if matches!(
            current.state,
            WordlistState::Pending | WordlistState::Published
        ) {
            return Err(conflict("请先撤回词表再修改名称"));
        }
        sqlx::query("UPDATE wordlists SET name=$2,state='draft',review_state='none',revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(w).bind(value).execute(&mut *tx).await.map_err(AppError::internal)?;
    } else {
        nodes[index].name = value;
    }
    validate_tree(&nodes)?;
    save(&mut tx, &nodes[index]).await?;
    commit(tx, auth).await
}
fn selection(
    nodes: &[Node],
    input: &LibraryOperation,
) -> Result<(Vec<Uuid>, HashMap<Uuid, Uuid>), AppError> {
    let trash = matches!(input.action, LibraryAction::Restore | LibraryAction::Purge);
    if input.empty_trash && input.action != LibraryAction::Purge {
        return Err(invalid("仅永久删除可清空回收站"));
    }
    let mut roots = if input.empty_trash {
        nodes
            .iter()
            .filter(|n| n.trash_root_id == Some(n.id))
            .map(|n| n.id)
            .collect()
    } else {
        input.ids.clone()
    };
    if (!input.empty_trash && (roots.is_empty() || roots.len() > 100))
        || roots.iter().collect::<HashSet<_>>().len() != roots.len()
    {
        return Err(invalid("请选择 1–100 个不同项目"));
    }
    for id in &roots {
        if find(nodes, *id)?.trashed_at.is_some() != trash {
            return Err(missing());
        }
    }
    let selected: HashSet<_> = roots.iter().copied().collect();
    roots.retain(|id| {
        !path(nodes, find(nodes, *id).ok().and_then(|n| n.parent_id))
            .unwrap_or_default()
            .iter()
            .any(|p| {
                selected.contains(&p.id)
                    && (!trash
                        || find(nodes, p.id).is_ok_and(|p| {
                            p.trash_root_id == find(nodes, *id).unwrap().trash_root_id
                        }))
            })
    });
    let mut included: HashMap<Uuid, Uuid> = roots.iter().map(|id| (*id, *id)).collect();
    loop {
        let mut added = false;
        for n in nodes {
            if included.contains_key(&n.id) || n.trashed_at.is_some() != trash {
                continue;
            }
            if let Some(root) = n.parent_id.and_then(|p| included.get(&p)).copied() {
                if trash && n.trash_root_id != find(nodes, root)?.trash_root_id {
                    continue;
                }
                included.insert(n.id, root);
                added = true;
            }
        }
        if !added {
            break;
        }
    }
    Ok((roots, included))
}
pub async fn operation(
    pool: &PgPool,
    auth: &AuthUser,
    space: LibrarySpace,
    input: LibraryOperation,
    preview: bool,
) -> Result<LibraryImpact, AppError> {
    let mut tx = begin(pool, auth).await?;
    let mut nodes = load(&mut tx, auth.subject, space).await?;
    check_version(&nodes, &input.expected_version)?;
    let (roots, included) = selection(&nodes, &input)?;
    let count = nodes
        .iter()
        .filter(|n| included.contains_key(&n.id))
        .count() as i64;
    let folders = nodes
        .iter()
        .filter(|n| included.contains_key(&n.id) && n.wordlist_id.is_none())
        .count() as i64;
    let impact = LibraryImpact {
        version: version(&nodes)?,
        folder_count: folders,
        wordlist_count: count - folders,
    };
    if preview {
        commit(tx, auth).await?;
        return Ok(impact);
    }
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    match input.action {
        LibraryAction::Move => {
            target(&nodes, input.target_id)?;
            if input.target_id.is_some_and(|id| included.contains_key(&id)) {
                return Err(invalid("不能移入自身或子文件夹"));
            }
            for n in &mut nodes {
                if roots.contains(&n.id) {
                    n.parent_id = input.target_id;
                }
            }
            validate_tree(&nodes)?;
        }
        LibraryAction::Trash => {
            for n in &mut nodes {
                if let Some(root) = included.get(&n.id) {
                    n.trashed_at = Some(now);
                    n.trash_root_id = Some(*root);
                }
            }
        }
        LibraryAction::Restore => {
            if !input.original_location {
                target(&nodes, input.target_id)?;
            }
            for id in &roots {
                let index = nodes.iter().position(|n| n.id == *id).ok_or_else(missing)?;
                let parent = if input.original_location {
                    nodes[index]
                        .parent_id
                        .filter(|id| target(&nodes, Some(*id)).is_ok())
                } else {
                    input.target_id
                };
                nodes[index].parent_id = parent;
            }
            for n in &mut nodes {
                if included.contains_key(&n.id) {
                    n.trashed_at = None;
                    n.trash_root_id = None;
                }
            }
            for index in 0..nodes.len() {
                if !included.contains_key(&nodes[index].id) || nodes[index].wordlist_id.is_some() {
                    continue;
                }
                let base = nodes[index].name.clone();
                let mut suffix = 0;
                while nodes.iter().any(|n| {
                    n.id != nodes[index].id
                        && n.trashed_at.is_none()
                        && n.wordlist_id.is_none()
                        && n.parent_id == nodes[index].parent_id
                        && n.name == nodes[index].name
                }) {
                    suffix += 1;
                    let end = if suffix == 1 {
                        "（恢复）".into()
                    } else {
                        format!("（恢复 {suffix}）")
                    };
                    nodes[index].name = format!(
                        "{}{}",
                        base.chars()
                            .take(100 - end.chars().count())
                            .collect::<String>(),
                        end
                    );
                }
            }
            validate_tree(&nodes)?;
        }
        LibraryAction::Purge => {}
    }
    // Wordlist locks precede node updates and are acquired in stable ID order.
    let mut wordlists: Vec<_> = nodes
        .iter()
        .filter(|n| included.contains_key(&n.id))
        .filter_map(|n| n.wordlist_id)
        .collect();
    wordlists.sort();
    for id in wordlists {
        if space == LibrarySpace::Owned && input.action != LibraryAction::Move {
            let deleted: Option<Option<DateTime<Utc>>> = sqlx::query_scalar(
                "SELECT deleted_at FROM wordlists WHERE id=$1 AND owner_user_id=$2 FOR UPDATE",
            )
            .bind(id)
            .bind(auth.subject)
            .fetch_optional(&mut *tx)
            .await
            .map_err(AppError::internal)?;
            let deleted = deleted.ok_or_else(missing)?;
            if (input.action == LibraryAction::Trash && deleted.is_some())
                || (input.action != LibraryAction::Trash && deleted.is_none())
            {
                return Err(conflict("词表状态已改变，请刷新"));
            }
            if input.action == LibraryAction::Purge {
                sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(AppError::internal)?;
                sqlx::query(
                    "UPDATE wordlists SET name='已删除词表',revision=revision+1 WHERE id=$1",
                )
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(AppError::internal)?;
            } else {
                let deleted_at = if input.action == LibraryAction::Trash {
                    let at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(AppError::internal)?;
                    deletion::record(&mut tx, id, at, DeletionActor::User(auth.subject)).await?;
                    Some(at)
                } else {
                    None
                };
                sqlx::query("UPDATE wordlist_review_requests SET state='cancelled',decided_at=clock_timestamp() WHERE wordlist_id=$1 AND state='pending'").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
                sqlx::query("UPDATE wordlists SET deleted_at=$2,state='draft',review_state='none',revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(deleted_at).execute(&mut *tx).await.map_err(AppError::internal)?;
            }
            super::custom_audio::reconcile_in(&mut tx, id).await?;
        } else if space == LibrarySpace::Favorites {
            if input.action == LibraryAction::Restore {
                sqlx::query("INSERT INTO wordlist_interactions(wordlist_id,user_id,kind) VALUES($1,$2,'favorite') ON CONFLICT DO NOTHING").bind(id).bind(auth.subject).execute(&mut *tx).await.map_err(AppError::internal)?;
            } else if matches!(input.action, LibraryAction::Trash | LibraryAction::Purge) {
                sqlx::query("DELETE FROM wordlist_interactions WHERE wordlist_id=$1 AND user_id=$2 AND kind='favorite'").bind(id).bind(auth.subject).execute(&mut *tx).await.map_err(AppError::internal)?;
            }
        }
    }
    if input.action == LibraryAction::Purge {
        for n in &mut nodes {
            if !included.contains_key(&n.id)
                && n.parent_id.is_some_and(|p| included.contains_key(&p))
            {
                n.parent_id = None;
                save(&mut tx, n).await?;
            }
        }
        let ids: Vec<_> = included.keys().copied().collect();
        sqlx::query("DELETE FROM wordlist_library_nodes WHERE id=ANY($1)")
            .bind(ids)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    } else {
        for n in &nodes {
            if included.contains_key(&n.id) {
                save(&mut tx, n).await?;
            }
        }
    }
    commit(tx, auth).await?;
    Ok(impact)
}

pub async fn expand_selection(
    pool: &PgPool,
    auth: &AuthUser,
    space: LibrarySpace,
    input: LibrarySelectionInput,
) -> Result<LibrarySelection, AppError> {
    if input.node_ids.is_empty() || input.node_ids.len() > 100 {
        return Err(invalid("请选择目录项目"));
    }
    // Resolve owners before any library/task/source locks, then validate the tree under its lock.
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT wordlist_id FROM wordlist_library_nodes WHERE user_id=$1 AND space=$2 AND wordlist_id IS NOT NULL").bind(auth.subject).bind(space.as_str()).fetch_all(pool).await.map_err(AppError::internal)?;
    let mut tx = crate::learning_tasks::repository::begin(pool, auth, &ids).await?;
    lock(&mut tx, auth.subject).await?;
    let nodes = load(&mut tx, auth.subject, space).await?;
    check_version(&nodes, &input.expected_version)?;
    if nodes
        .iter()
        .filter_map(|n| n.wordlist_id)
        .any(|id| !ids.contains(&id))
    {
        return Err(conflict("目录已变化，请刷新后重新选择"));
    }
    let mut chosen = HashSet::new();
    for id in &input.node_ids {
        let root = find(&nodes, *id)?;
        if root.trashed_at.is_some() {
            return Err(invalid("回收站内容不能用于学习任务"));
        }
        let descendants: Vec<_> = nodes
            .iter()
            .filter(|n| {
                n.trashed_at.is_none()
                    && (n.id == *id
                        || path(&nodes, n.parent_id).is_ok_and(|p| p.iter().any(|a| a.id == *id)))
            })
            .filter_map(|n| n.wordlist_id)
            .collect();
        if descendants.is_empty() {
            return Err(invalid("所选文件夹没有词表，请调整选择"));
        }
        chosen.extend(descendants);
    }
    if chosen.len() > 5 {
        return Err(invalid("展开后超过 5 份词表，请减少选择"));
    }
    let mut ids: Vec<_> = chosen.into_iter().collect();
    ids.sort();
    let access = crate::learning_tasks::repository::sources(&mut tx, auth, &ids).await?;
    if access.lists.len() != ids.len() {
        return Err(conflict("包含不可用来源，请逐份选择可用词表"));
    }
    let wordlists = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{} WHERE w.id=ANY($1) ORDER BY w.name,w.id",
        service::META
    )))
    .bind(ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let version = version(&nodes)?;
    commit(tx, auth).await?;
    Ok(LibrarySelection { version, wordlists })
}
