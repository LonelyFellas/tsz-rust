use super::{dto::*, projection};
use crate::{
    api::PaginationMeta,
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::HashSet;
use uuid::Uuid;

pub type Tx<'a> = Transaction<'a, Postgres>;
// SQL interpolation below is limited to these fixed fragments and a boolean lock mode.
// All request values are bind parameters.
pub const VISIBLE_OWNER: &str = "(w.owner_admin_id IS NOT NULL OR (u.status='active' AND NOT EXISTS(SELECT 1 FROM account_deletion_requests d WHERE d.user_id=u.id AND d.status='pending' AND d.effective_at<=clock_timestamp())))";
pub const META: &str = "SELECT w.id,w.owner_user_id,COALESCE(u.display_name,a.display_name) AS owner_name,w.name,w.state,w.revision,(SELECT count(*) FROM wordlist_items i WHERE i.wordlist_id=w.id) AS item_count,w.created_at,w.updated_at FROM wordlists w LEFT JOIN users u ON u.id=w.owner_user_id LEFT JOIN admins a ON a.id=w.owner_admin_id";
fn invalid(message: &str) -> AppError {
    AppError::bad_request(ErrorCode::ValidationFailed, message)
}
pub fn conflict() -> AppError {
    AppError::conflict(ErrorCode::WordListConflict, None, "词表已改变，请重新加载")
}
pub fn pagination(query: &WordlistQuery) -> Result<(u32, u32, String), AppError> {
    let page = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(50);
    let q = query.q.as_deref().unwrap_or("").trim();
    if page == 0 || !(1..=100).contains(&size) || q.chars().count() > 100 {
        return Err(AppError::bad_request(
            ErrorCode::InvalidQuery,
            "invalid pagination or search",
        ));
    }
    Ok((page, size, q.to_owned()))
}
pub fn page_meta(page: u32, size: u32, total: i64) -> PaginationMeta {
    PaginationMeta {
        page,
        page_size: size,
        total,
        total_pages: (total + i64::from(size) - 1) / i64::from(size),
    }
}
pub async fn lock_user(tx: &mut Tx<'_>, auth: &AuthUser) -> Result<(), AppError> {
    let valid: Option<bool> = sqlx::query_scalar(
        "SELECT true FROM users WHERE id=$1 AND status='active' AND security_version=$2 FOR SHARE",
    )
    .bind(auth.subject)
    .bind(auth.security_version)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    if valid != Some(true) {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    crate::account_deletion::ensure_not_effective_in(tx, auth.subject).await
}
pub async fn eligible_owner(tx: &mut Tx<'_>, owner: Option<Uuid>) -> Result<(), AppError> {
    let Some(owner) = owner else {
        return Ok(());
    };
    let valid: Option<bool> =
        sqlx::query_scalar("SELECT status='active' FROM users WHERE id=$1 FOR SHARE")
            .bind(owner)
            .fetch_optional(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    if valid != Some(true)
        || crate::account_deletion::is_effective_in(tx, owner)
            .await
            .map_err(AppError::internal)?
    {
        return Err(AppError::not_found("wordlist not found"));
    }
    Ok(())
}
pub async fn lock_list(
    tx: &mut Tx<'_>,
    id: Uuid,
    owner: Option<Uuid>,
    write: bool,
) -> Result<Wordlist, AppError> {
    lock_list_metadata(tx, id, owner, write).await?;
    meta(tx, id).await
}
async fn lock_list_metadata(
    tx: &mut Tx<'_>,
    id: Uuid,
    owner: Option<Uuid>,
    write: bool,
) -> Result<(Option<Uuid>, WordlistState, i64), AppError> {
    let sql = if write {
        "SELECT owner_user_id,state,revision FROM wordlists WHERE id=$1 AND deleted_at IS NULL AND ($2::uuid IS NULL OR owner_user_id=$2) FOR UPDATE"
    } else {
        "SELECT owner_user_id,state,revision FROM wordlists WHERE id=$1 AND deleted_at IS NULL AND ($2::uuid IS NULL OR owner_user_id=$2) FOR SHARE"
    };
    sqlx::query_as(sql)
        .bind(id)
        .bind(owner)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("wordlist not found"))
}
pub async fn meta(tx: &mut Tx<'_>, id: Uuid) -> Result<Wordlist, AppError> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{META} WHERE w.id=$1 AND w.deleted_at IS NULL"
    )))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("wordlist not found"))
}
pub fn validate_content(name: &str, ids: &[Uuid]) -> Result<(), AppError> {
    if name.trim().is_empty()
        || name.chars().count() > 100
        || ids.is_empty()
        || ids.len() > 10000
        || ids.iter().collect::<HashSet<_>>().len() != ids.len()
    {
        return Err(invalid("名称须为 1–100 字，词条须为 1–10000 个且不重复"));
    }
    Ok(())
}
pub async fn lock_entries(
    tx: &mut Tx<'_>,
    ids: &[Uuid],
    require_all: bool,
) -> Result<(), AppError> {
    let locked = crate::lexicon::published::lock_entries_in(tx, ids).await?;
    if require_all {
        let supported:i64=sqlx::query_scalar("SELECT count(*) FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=ANY($1) AND e.archived_at IS NULL AND p.content_schema_version=3").bind(ids).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
        if locked != ids.len() || supported as usize != ids.len() {
            return Err(invalid("包含未发布、归档或不支持的词条"));
        }
    }
    Ok(())
}
pub async fn ids(tx: &mut Tx<'_>, id: Uuid) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1 AND entry_id IS NOT NULL ORDER BY position")
        .bind(id)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)
}
pub fn hash(input: &impl serde::Serialize) -> Result<Vec<u8>, AppError> {
    Ok(Sha256::digest(serde_json::to_vec(input).map_err(AppError::internal)?).to_vec())
}
pub fn validate_items(name: &str, items: &[WordlistContentItem]) -> Result<(), AppError> {
    validate_content(name, &items.iter().map(|i| i.item_id).collect::<Vec<_>>())?;
    let mut entries = HashSet::new();
    for item in items {
        if item.author_note.chars().count() > 1000 {
            return Err(invalid("注不能超过 1000 字"));
        }
        match (&item.entry_id, &item.custom_word) {
            (Some(entry), None) if item.item_id == *entry && entries.insert(*entry) => {}
            (None, Some(custom))
                if !custom.word.trim().is_empty()
                    && custom.word.chars().count() <= 50
                    && !custom.definition.trim().is_empty()
                    && custom.definition.chars().count() <= 200 => {}
            _ => {
                return Err(invalid(
                    "请选择平台词条，或填写自定义词汇（1–50 字）及释义（1–200 字）",
                ));
            }
        }
    }
    Ok(())
}
type ContentRow = (
    Uuid,
    Option<Uuid>,
    Option<sqlx::types::Json<CustomWord>>,
    String,
);
pub async fn content_items(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Vec<WordlistContentItem>, AppError> {
    let rows: Vec<ContentRow> = sqlx::query_as(
        "SELECT item_id,entry_id,custom_word,author_note FROM wordlist_items WHERE wordlist_id=$1 ORDER BY position"
    ).bind(id).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    Ok(rows
        .into_iter()
        .map(
            |(item_id, entry_id, custom_word, author_note)| WordlistContentItem {
                item_id,
                entry_id,
                custom_word: custom_word.map(|v| v.0),
                author_note,
            },
        )
        .collect())
}
async fn write_items(
    tx: &mut Tx<'_>,
    id: Uuid,
    items: &[WordlistContentItem],
) -> Result<(), AppError> {
    let entry_ids: Vec<_> = items.iter().filter_map(|i| i.entry_id).collect();
    lock_entries(tx, &entry_ids, true).await?;
    let keys: Vec<_> = items.iter().map(|i| i.item_id).collect();
    // Preserve retained rows (including private notes and learning membership IDs).
    // A stable item can change text but cannot change source or platform identity.
    let old: std::collections::HashMap<_, _> = content_items(tx, id)
        .await?
        .into_iter()
        .map(|i| (i.item_id, i.entry_id))
        .collect();
    if items.iter().any(|n| {
        old.get(&n.item_id)
            .is_some_and(|entry| *entry != n.entry_id)
    }) {
        return Err(invalid("词条来源不能直接修改，请删除后重新添加"));
    }
    sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1 AND NOT(item_id=ANY($2))")
        .bind(id)
        .bind(keys)
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("INSERT INTO wordlist_items(wordlist_id,item_id,entry_id,custom_word,position,author_note) SELECT $1,(x.value->>'item_id')::uuid,(x.value->>'entry_id')::uuid,x.value->'custom_word',(x.n-1)::integer,x.value->>'author_note' FROM jsonb_array_elements($2::jsonb) WITH ORDINALITY AS x(value,n) ON CONFLICT(wordlist_id,item_id) DO UPDATE SET position=excluded.position,custom_word=excluded.custom_word,author_note=excluded.author_note")
        .bind(id).bind(serde_json::to_value(items).map_err(AppError::internal)?).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
pub async fn create(
    state: &crate::state::AppState,
    auth: &AuthUser,
    input: CreateWordlist,
    headers: &axum::http::HeaderMap,
) -> Result<Wordlist, AppError> {
    let pool = &state.pool;
    let items: Vec<_> = input
        .items
        .iter()
        .map(|i| {
            Ok(WordlistContentItem {
                item_id: i
                    .entry_id
                    .or(i.item_id)
                    .ok_or_else(|| invalid("自定义词缺少稳定标识"))?,
                entry_id: i.entry_id,
                custom_word: i.custom_word.clone(),
                author_note: i.author_note.clone(),
            })
        })
        .collect::<Result<_, AppError>>()?;
    validate_items(&input.name, &items)?;
    if input.items.iter().any(|i| {
        i.private_note.chars().count() > 1000
            || i.entry_id
                .zip(i.item_id)
                .is_some_and(|(entry, item)| entry != item)
    }) {
        return Err(invalid("备注不能超过 1000 字，平台词条标识须一致"));
    }
    let request_hash = hash(&input)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
    super::library::lock(&mut tx, auth.subject).await?;
    // Serialize the creator's request key before taking entry locks (including retries).
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!(
            "wordlist-create:{}:{}",
            auth.subject, input.idempotency_key
        ))
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let existing: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT id,create_hash FROM wordlists WHERE owner_user_id=$1 AND create_key=$2",
    )
    .bind(auth.subject)
    .bind(input.idempotency_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let id = if let Some((id, old_hash)) = existing {
        if old_hash != request_hash {
            return Err(AppError::conflict(
                ErrorCode::IdempotencyConflict,
                None,
                "创建请求键已用于其他内容",
            ));
        }
        id
    } else {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO wordlists(id,owner_user_id,name,create_key,create_hash) VALUES($1,$2,$3,$4,$5)").bind(id).bind(auth.subject).bind(input.name.trim()).bind(input.idempotency_key).bind(request_hash).execute(&mut *tx).await.map_err(AppError::internal)?;
        write_items(&mut tx, id, &items).await?;
        if let Some(request) = &input.audio_generation {
            super::custom_audio::generate_in(&mut tx, state, auth, headers, id, request).await?;
        }
        let item_ids: Vec<_> = items.iter().map(|i| i.item_id).collect();
        let notes: Vec<_> = input
            .items
            .iter()
            .map(|i| i.private_note.as_str())
            .collect();
        sqlx::query("UPDATE wordlist_items i SET private_note=x.note FROM unnest($2::uuid[],$3::text[]) AS x(id,note) WHERE i.wordlist_id=$1 AND i.item_id=x.id")
            .bind(id).bind(item_ids).bind(notes).execute(&mut *tx).await.map_err(AppError::internal)?;
        super::library::add_wordlist(
            &mut tx,
            auth.subject,
            super::library::dto::LibrarySpace::Owned,
            id,
            input.folder_id,
        )
        .await?;
        id
    };
    let result = meta(&mut tx, id).await?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn update(
    state: &crate::state::AppState,
    auth: &AuthUser,
    id: Uuid,
    input: UpdateWordlist,
    headers: &axum::http::HeaderMap,
) -> Result<Wordlist, AppError> {
    let pool = &state.pool;
    if input.audio_generation.is_some() && input.idempotency_key.is_none() {
        return Err(invalid("生成语音的保存请求缺少请求标识"));
    }
    let request_hash = hash(&input)?;
    if input.note_updates.len() > 10000
        || input.note_updates.iter().any(|n| {
            n.private_note.chars().count() > 1000
                || n.expected_note_revision < 1
                || n.item_id.or(n.entry_id).is_none()
                || (n.item_id.is_some() && n.entry_id.is_some())
        })
        || input
            .note_updates
            .iter()
            .map(|n| n.item_id.or(n.entry_id))
            .collect::<HashSet<_>>()
            .len()
            != input.note_updates.len()
    {
        return Err(invalid("invalid note updates"));
    }
    if let Some(c) = &input.content {
        if let Some(items) = &c.items {
            if !c.entry_ids.is_empty() || c.author_notes.is_some() {
                return Err(invalid("items 不可与旧版选词字段混用"));
            }
            validate_items(&c.name, items)?;
        } else {
            validate_content(&c.name, &c.entry_ids)?;
        }
        if let Some(notes) = &c.author_notes
            && (notes.len() != c.entry_ids.len()
                || notes.iter().zip(&c.entry_ids).any(|(note, id)| {
                    note.entry_id != *id || note.author_note.chars().count() > 1000
                }))
        {
            return Err(invalid("作者注释须与词条顺序一致，且每条不超过 1000 字"));
        }
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
    super::library::lock(&mut tx, auth.subject).await?;
    let current = lock_list(&mut tx, id, Some(auth.subject), true).await?;
    if let Some(key) = input.idempotency_key {
        let stored: Option<(Uuid, Vec<u8>, serde_json::Value)> = sqlx::query_as("SELECT wordlist_id,request_hash,response FROM wordlist_update_requests WHERE user_id=$1 AND request_key=$2")
            .bind(auth.subject).bind(key).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
        if let Some((target, previous_hash, response)) = stored {
            if target != id || previous_hash != request_hash {
                return Err(AppError::conflict(
                    ErrorCode::IdempotencyConflict,
                    None,
                    "保存请求键已用于其他内容",
                ));
            }
            return serde_json::from_value(response).map_err(AppError::internal);
        }
    }
    if current.revision != input.expected_revision {
        return Err(conflict());
    }
    if let Some(c) = &input.content {
        if matches!(
            current.state,
            WordlistState::Pending | WordlistState::Published
        ) {
            return Err(AppError::conflict(
                ErrorCode::WordListConflict,
                None,
                "请先撤回词表再修改内容",
            ));
        }
        if current.name != c.name.trim() {
            sqlx::query("UPDATE wordlist_library_nodes SET revision=revision+1 WHERE user_id=$1 AND space='owned' AND wordlist_id=$2")
                .bind(auth.subject).bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
        }
        let items = if let Some(items) = &c.items {
            items.clone()
        } else {
            let old = content_items(&mut tx, id).await?;
            if old.iter().any(|i| i.custom_word.is_some()) {
                return Err(AppError::conflict(
                    ErrorCode::WordListConflict,
                    None,
                    "此词表包含自定义词汇，请刷新页面后重新编辑",
                ));
            }
            let notes: std::collections::HashMap<_, _> = old
                .into_iter()
                .map(|i| (i.item_id, i.author_note))
                .collect();
            c.entry_ids
                .iter()
                .enumerate()
                .map(|(position, entry)| WordlistContentItem {
                    item_id: *entry,
                    entry_id: Some(*entry),
                    custom_word: None,
                    author_note: c
                        .author_notes
                        .as_ref()
                        .map(|notes| notes[position].author_note.clone())
                        .unwrap_or_else(|| notes.get(entry).cloned().unwrap_or_default()),
                })
                .collect()
        };
        write_items(&mut tx, id, &items).await?;
        sqlx::query("UPDATE wordlist_review_requests SET state='cancelled',decided_at=clock_timestamp() WHERE wordlist_id=$1 AND state='pending'").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("UPDATE wordlists SET name=$2,state='draft',review_state='none',revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(c.name.trim()).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    super::custom_audio::reconcile_in(&mut tx, id).await?;
    if let Some(request) = &input.audio_generation {
        if matches!(
            current.state,
            WordlistState::Pending | WordlistState::Published
        ) {
            return Err(AppError::conflict(
                ErrorCode::WordListConflict,
                None,
                "请先撤回词表再生成语音",
            ));
        }
        super::custom_audio::generate_in(&mut tx, state, auth, headers, id, request).await?;
        if input.content.is_none() && !request.item_ids.is_empty() {
            sqlx::query(
                "UPDATE wordlists SET revision=revision+1,updated_at=clock_timestamp() WHERE id=$1",
            )
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        }
    }
    for note in input.note_updates {
        let result=sqlx::query("UPDATE wordlist_items SET private_note=$3,note_revision=note_revision+1 WHERE wordlist_id=$1 AND item_id=$2 AND note_revision=$4").bind(id).bind(note.item_id.or(note.entry_id)).bind(note.private_note).bind(note.expected_note_revision).execute(&mut *tx).await.map_err(AppError::internal)?;
        if result.rows_affected() != 1 {
            return Err(conflict());
        }
    }
    let result = meta(&mut tx, id).await?;
    if let Some(key) = input.idempotency_key {
        sqlx::query("INSERT INTO wordlist_update_requests(user_id,request_key,wordlist_id,request_hash,response) VALUES($1,$2,$3,$4,$5)")
            .bind(auth.subject).bind(key).bind(id).bind(request_hash).bind(serde_json::to_value(&result).map_err(AppError::internal)?)
            .execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn list(
    pool: &PgPool,
    owner: Option<&AuthUser>,
    query: WordlistQuery,
) -> Result<WordlistPage, AppError> {
    let (page, size, q) = pagination(&query)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    if let Some(auth) = owner {
        lock_user(&mut tx, auth).await?;
    }
    let filter = format!(
        "w.deleted_at IS NULL AND ($1::uuid IS NOT NULL AND w.owner_user_id=$1 OR $1::uuid IS NULL AND w.state='published' AND {VISIBLE_OWNER}) AND strpos(lower(w.name),lower($2))>0"
    );
    let total = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM wordlists w LEFT JOIN users u ON u.id=w.owner_user_id LEFT JOIN admins a ON a.id=w.owner_admin_id WHERE {filter}"
    )))
    .bind(owner.map(|a| a.subject))
    .bind(&q)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let items = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{META} WHERE {filter} ORDER BY w.created_at DESC,w.id DESC LIMIT $3 OFFSET $4"
    )))
    .bind(owner.map(|a| a.subject))
    .bind(q)
    .bind(i64::from(size))
    .bind(i64::from(page - 1) * i64::from(size))
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistPage {
        items,
        pagination: page_meta(page, size, total),
    })
}
pub async fn read_lock(
    tx: &mut Tx<'_>,
    id: Uuid,
    auth: Option<&AuthUser>,
) -> Result<Wordlist, AppError> {
    read_lock_metadata(tx, id, auth).await?;
    meta(tx, id).await
}
async fn read_lock_metadata(
    tx: &mut Tx<'_>,
    id: Uuid,
    auth: Option<&AuthUser>,
) -> Result<(Option<Uuid>, WordlistState, i64), AppError> {
    if let Some(auth) = auth {
        lock_user(tx, auth).await?;
        return lock_list_metadata(tx, id, Some(auth.subject), false).await;
    }
    let owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT owner_user_id FROM wordlists WHERE id=$1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("wordlist not found"))?;
    eligible_owner(tx, owner).await?;
    let metadata = lock_list_metadata(tx, id, None, false).await?;
    if metadata.1 != WordlistState::Published {
        return Err(AppError::not_found("wordlist not found"));
    }
    Ok(metadata)
}
pub async fn read(pool: &PgPool, id: Uuid, auth: Option<&AuthUser>) -> Result<Wordlist, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let result = read_lock(&mut tx, id, auth).await?;
    eligible_owner(&mut tx, result.owner_user_id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn availability(
    pool: &PgPool,
    id: Uuid,
    auth: Option<&AuthUser>,
) -> Result<WordlistAvailability, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let (owner, _, revision) = read_lock_metadata(&mut tx, id, auth).await?;
    let (available_count, unavailable_count, custom_count) = sqlx::query_as(
        "SELECT count(*) FILTER(WHERE p.id IS NOT NULL OR i.custom_word IS NOT NULL),count(*) FILTER(WHERE p.id IS NULL AND i.custom_word IS NULL),count(*) FILTER(WHERE i.custom_word IS NOT NULL) FROM wordlist_items i LEFT JOIN lexicon.entries e ON e.id=i.entry_id AND e.archived_at IS NULL LEFT JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id AND p.content_schema_version=3 WHERE i.wordlist_id=$1",
    ).bind(id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    eligible_owner(&mut tx, owner).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistAvailability {
        revision,
        custom_count: Some(custom_count),
        available_count,
        unavailable_count,
    })
}

pub async fn edit_snapshot(
    pool: &PgPool,
    id: Uuid,
    auth: &AuthUser,
) -> Result<WordlistEditSnapshot, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let wordlist = read_lock(&mut tx, id, Some(auth)).await?;
    let entry_ids = ids(&mut tx, id).await?;
    let items = Some(content_items(&mut tx, id).await?);
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistEditSnapshot {
        wordlist,
        entry_ids,
        items,
    })
}
#[derive(sqlx::FromRow)]
pub struct ItemRow {
    pub item_id: Uuid,
    pub entry_id: Option<Uuid>,
    pub custom_word: Option<sqlx::types::Json<CustomWord>>,
    pub position: i32,
    pub author_note: String,
    pub private_note: String,
    pub note_revision: i64,
}
pub(super) fn public_items(
    rows: Vec<ItemRow>,
    mut entries: std::collections::HashMap<Uuid, WordlistEntry>,
) -> Vec<WordlistItem> {
    rows.into_iter()
        .map(|r| WordlistItem {
            entry_id: r.entry_id,
            item_id: Some(r.item_id),
            custom_word: r.custom_word.map(|v| v.0),
            position: r.position,
            author_note: Some(r.author_note),
            entry: r.entry_id.and_then(|id| entries.remove(&id)),
        })
        .collect()
}
pub async fn read_entries(
    tx: &mut Tx<'_>,
    entry_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, WordlistEntry>, AppError> {
    read_entries_view(tx, entry_ids, WordlistView::Standard).await
}
pub async fn read_entries_view(
    tx: &mut Tx<'_>,
    entry_ids: &[Uuid],
    view: WordlistView,
) -> Result<std::collections::HashMap<Uuid, WordlistEntry>, AppError> {
    lock_entries(tx, entry_ids, false).await?;
    let form_labels = if view == WordlistView::Full {
        let labels: Vec<(String, String)> =
            sqlx::query_as("SELECT code, name_zh FROM catalog.form_types")
                .fetch_all(&mut **tx)
                .await
                .map_err(AppError::internal)?;
        let pos: Vec<(String, String)> =
            sqlx::query_as("SELECT code, name_zh FROM catalog.parts_of_speech")
                .fetch_all(&mut **tx)
                .await
                .map_err(AppError::internal)?;
        let sub_pos: Vec<(String, String, String)> = sqlx::query_as("SELECT p.code,s.code,s.name_zh FROM catalog.sub_parts_of_speech s JOIN catalog.parts_of_speech p ON p.id=s.part_of_speech_id").fetch_all(&mut **tx).await.map_err(AppError::internal)?;
        Some(projection::ReadingLabels {
            forms: labels.into_iter().collect(),
            pos: pos.into_iter().collect(),
            sub_pos: sub_pos
                .into_iter()
                .map(|(pos, code, label)| ((pos, code), label))
                .collect(),
        })
    } else {
        None
    };
    let rows:Vec<(Uuid,Uuid,serde_json::Value)>=sqlx::query_as("SELECT e.id,p.id,p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=ANY($1) AND e.archived_at IS NULL AND p.content_schema_version=3").bind(entry_ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    rows.into_iter()
        .map(|(id, p, s)| Ok((id, projection::project(id, p, s, form_labels.as_ref())?)))
        .collect()
}
pub async fn unavailable_reasons(
    tx: &mut Tx<'_>,
    entry_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, WordlistUnavailableReason>, AppError> {
    let rows: Vec<(Uuid, bool, Option<i16>)> = sqlx::query_as("SELECT e.id,e.archived_at IS NOT NULL,p.content_schema_version FROM lexicon.entries e LEFT JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=ANY($1)")
        .bind(entry_ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, archived, schema)| {
            let reason = match (archived, schema) {
                (true, _) => WordlistUnavailableReason::Archived,
                (false, None) => WordlistUnavailableReason::Unpublished,
                (false, Some(3)) => return None,
                (false, Some(_)) => WordlistUnavailableReason::UnsupportedSchema,
            };
            Some((id, reason))
        })
        .collect())
}

pub async fn item_rows(
    tx: &mut Tx<'_>,
    id: Uuid,
    query: &WordlistItemsQuery,
) -> Result<(Vec<ItemRow>, PaginationMeta), AppError> {
    let (page, size, q) = pagination(&WordlistQuery {
        q: query.q.clone(),
        page: query.page,
        page_size: query.page_size,
    })?;
    // Use only visible current publication labels for both matching and ordering.
    let from = "wordlist_items i LEFT JOIN lexicon.entries e ON e.id=i.entry_id AND e.archived_at IS NULL LEFT JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id AND p.content_schema_version=3";
    let filter = "i.wordlist_id=$1 AND ($2='' OR strpos(lower(COALESCE(i.custom_word->>'word',p.snapshot#>>'{presentation,label}')),lower($2))>0)";
    let total = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {from} WHERE {filter}"
    )))
    .bind(id)
    .bind(&q)
    .fetch_one(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    let order = match query.sort {
        WordlistSort::Author => "i.position",
        WordlistSort::LabelAsc => {
            "lower(COALESCE(i.custom_word->>'word',p.snapshot#>>'{presentation,label}')) COLLATE \"C\" ASC NULLS LAST,i.position"
        }
        WordlistSort::LabelDesc => {
            "lower(COALESCE(i.custom_word->>'word',p.snapshot#>>'{presentation,label}')) COLLATE \"C\" DESC NULLS LAST,i.position"
        }
    };
    let rows=sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT i.item_id,i.entry_id,i.custom_word,i.position,i.author_note,i.private_note,i.note_revision FROM {from} WHERE {filter} ORDER BY {order} LIMIT $3 OFFSET $4"))).bind(id).bind(q).bind(i64::from(size)).bind(i64::from(page-1)*i64::from(size)).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    Ok((rows, page_meta(page, size, total)))
}
pub async fn catalog(
    pool: &PgPool,
    auth: &AuthUser,
    query: WordlistQuery,
) -> Result<WordlistCatalog, AppError> {
    let (page, size, q) = pagination(&query)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
    let result = catalog_in(&mut tx, page, size, q).await?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn catalog_in(
    tx: &mut Tx<'_>,
    page: u32,
    size: u32,
    q: String,
) -> Result<WordlistCatalog, AppError> {
    let filter = "e.archived_at IS NULL AND p.content_schema_version=3 AND (strpos(lower(p.snapshot#>>'{presentation,label}'),lower($1))>0 OR (char_length($1)>=3 AND similarity(lower(p.snapshot#>>'{presentation,label}'),lower($1))>=0.3))";
    let total=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE {filter}"))).bind(&q).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
    let entry_ids:Vec<Uuid>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT e.id FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE {filter} ORDER BY (lower(p.snapshot#>>'{{presentation,label}}')=lower($1)) DESC,(strpos(lower(p.snapshot#>>'{{presentation,label}}'),lower($1))=1) DESC,similarity(lower(p.snapshot#>>'{{presentation,label}}'),lower($1)) DESC,p.snapshot#>>'{{presentation,label}}',e.id LIMIT $2 OFFSET $3"))).bind(q).bind(i64::from(size)).bind(i64::from(page-1)*i64::from(size)).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let mut entries = read_entries(tx, &entry_ids).await?;
    let items = entry_ids
        .iter()
        .filter_map(|id| entries.remove(id))
        .map(projection::candidate)
        .collect();
    Ok(WordlistCatalog {
        items,
        pagination: page_meta(page, size, total),
    })
}
