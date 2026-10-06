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
pub const VISIBLE_OWNER: &str = "u.status='active' AND NOT EXISTS(SELECT 1 FROM account_deletion_requests d WHERE d.user_id=u.id AND d.status='pending' AND d.effective_at<=clock_timestamp())";
const META: &str = "SELECT w.id,w.owner_user_id,u.display_name AS owner_name,w.name,w.state,w.revision,(SELECT count(*) FROM wordlist_items i WHERE i.wordlist_id=w.id) AS item_count,w.created_at,w.updated_at FROM wordlists w JOIN users u ON u.id=w.owner_user_id";
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
pub async fn eligible_owner(tx: &mut Tx<'_>, owner: Uuid) -> Result<(), AppError> {
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
    let sql = if write {
        "SELECT id FROM wordlists WHERE id=$1 AND ($2::uuid IS NULL OR owner_user_id=$2) FOR UPDATE"
    } else {
        "SELECT id FROM wordlists WHERE id=$1 AND ($2::uuid IS NULL OR owner_user_id=$2) FOR SHARE"
    };
    sqlx::query_scalar::<_, Uuid>(sql)
        .bind(id)
        .bind(owner)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("wordlist not found"))?;
    meta(tx, id).await
}
pub async fn meta(tx: &mut Tx<'_>, id: Uuid) -> Result<Wordlist, AppError> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{META} WHERE w.id=$1")))
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)
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
    let rows:Vec<(Uuid,Option<Uuid>,Option<chrono::DateTime<chrono::Utc>>)>=sqlx::query_as("SELECT id,current_publication_id,archived_at FROM lexicon.entries WHERE id=ANY($1) ORDER BY id FOR SHARE")
        .bind(ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    if require_all {
        let supported:i64=sqlx::query_scalar("SELECT count(*) FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=ANY($1) AND e.archived_at IS NULL AND p.content_schema_version=3").bind(ids).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
        if rows.len() != ids.len() || supported as usize != ids.len() {
            return Err(invalid("包含未发布、归档或不支持的词条"));
        }
    }
    Ok(())
}
pub async fn ids(tx: &mut Tx<'_>, id: Uuid) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1 ORDER BY position")
        .bind(id)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)
}
pub fn hash(input: &impl serde::Serialize) -> Result<Vec<u8>, AppError> {
    Ok(Sha256::digest(serde_json::to_vec(input).map_err(AppError::internal)?).to_vec())
}
pub async fn create(
    pool: &PgPool,
    auth: &AuthUser,
    input: CreateWordlist,
) -> Result<Wordlist, AppError> {
    let entry_ids: Vec<_> = input.items.iter().map(|i| i.entry_id).collect();
    validate_content(&input.name, &entry_ids)?;
    if input
        .items
        .iter()
        .any(|i| i.private_note.chars().count() > 1000)
    {
        return Err(invalid("备注不能超过 1000 字"));
    }
    let request_hash = hash(&input)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
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
        lock_entries(&mut tx, &entry_ids, true).await?;
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO wordlists(id,owner_user_id,name,create_key,create_hash) VALUES($1,$2,$3,$4,$5)").bind(id).bind(auth.subject).bind(input.name.trim()).bind(input.idempotency_key).bind(request_hash).execute(&mut *tx).await.map_err(AppError::internal)?;
        let notes: Vec<_> = input
            .items
            .iter()
            .map(|i| i.private_note.as_str())
            .collect();
        sqlx::query("INSERT INTO wordlist_items(wordlist_id,entry_id,position,private_note) SELECT $1,x.id,(x.n-1)::integer,x.note FROM unnest($2::uuid[],$3::text[]) WITH ORDINALITY AS x(id,note,n)").bind(id).bind(&entry_ids).bind(notes).execute(&mut *tx).await.map_err(AppError::internal)?;
        id
    };
    let result = meta(&mut tx, id).await?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn update(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: UpdateWordlist,
) -> Result<Wordlist, AppError> {
    if input.note_updates.len() > 10000
        || input
            .note_updates
            .iter()
            .any(|n| n.private_note.chars().count() > 1000 || n.expected_note_revision < 1)
        || input
            .note_updates
            .iter()
            .map(|n| n.entry_id)
            .collect::<HashSet<_>>()
            .len()
            != input.note_updates.len()
    {
        return Err(invalid("invalid note updates"));
    }
    if let Some(c) = &input.content {
        validate_content(&c.name, &c.entry_ids)?;
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
    let current = lock_list(&mut tx, id, Some(auth.subject), true).await?;
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
        lock_entries(&mut tx, &c.entry_ids, true).await?;
        sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1 AND NOT(entry_id=ANY($2))")
            .bind(id)
            .bind(&c.entry_ids)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        sqlx::query("INSERT INTO wordlist_items(wordlist_id,entry_id,position) SELECT $1,x.id,(x.n-1)::integer FROM unnest($2::uuid[]) WITH ORDINALITY AS x(id,n) ON CONFLICT(wordlist_id,entry_id) DO UPDATE SET position=excluded.position").bind(id).bind(&c.entry_ids).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("UPDATE wordlists SET name=$2,state='draft',revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(c.name.trim()).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    for note in input.note_updates {
        let result=sqlx::query("UPDATE wordlist_items SET private_note=$3,note_revision=note_revision+1 WHERE wordlist_id=$1 AND entry_id=$2 AND note_revision=$4").bind(id).bind(note.entry_id).bind(note.private_note).bind(note.expected_note_revision).execute(&mut *tx).await.map_err(AppError::internal)?;
        if result.rows_affected() != 1 {
            return Err(conflict());
        }
    }
    let result = meta(&mut tx, id).await?;
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
        "($1::uuid IS NOT NULL AND w.owner_user_id=$1 OR $1::uuid IS NULL AND w.state='published' AND {VISIBLE_OWNER}) AND strpos(lower(w.name),lower($2))>0"
    );
    let total = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM wordlists w JOIN users u ON u.id=w.owner_user_id WHERE {filter}"
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
    if let Some(auth) = auth {
        lock_user(tx, auth).await?;
        return lock_list(tx, id, Some(auth.subject), false).await;
    }
    let owner: Uuid = sqlx::query_scalar("SELECT owner_user_id FROM wordlists WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("wordlist not found"))?;
    eligible_owner(tx, owner).await?;
    let list = lock_list(tx, id, None, false).await?;
    if list.state != WordlistState::Published {
        return Err(AppError::not_found("wordlist not found"));
    }
    Ok(list)
}
pub async fn read(pool: &PgPool, id: Uuid, auth: Option<&AuthUser>) -> Result<Wordlist, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let result = read_lock(&mut tx, id, auth).await?;
    eligible_owner(&mut tx, result.owner_user_id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn edit_snapshot(
    pool: &PgPool,
    id: Uuid,
    auth: &AuthUser,
) -> Result<WordlistEditSnapshot, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let wordlist = read_lock(&mut tx, id, Some(auth)).await?;
    let entry_ids = ids(&mut tx, id).await?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistEditSnapshot {
        wordlist,
        entry_ids,
    })
}
#[derive(sqlx::FromRow)]
pub struct ItemRow {
    pub entry_id: Uuid,
    pub position: i32,
    pub private_note: String,
    pub note_revision: i64,
}
pub async fn read_entries(
    tx: &mut Tx<'_>,
    entry_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, WordlistEntry>, AppError> {
    lock_entries(tx, entry_ids, false).await?;
    let rows:Vec<(Uuid,Uuid,serde_json::Value)>=sqlx::query_as("SELECT e.id,p.id,p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=ANY($1) AND e.archived_at IS NULL AND p.content_schema_version=3").bind(entry_ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    rows.into_iter()
        .map(|(id, p, s)| Ok((id, projection::project(id, p, s)?)))
        .collect()
}
pub async fn item_rows(
    tx: &mut Tx<'_>,
    id: Uuid,
    query: &WordlistQuery,
) -> Result<(Vec<ItemRow>, PaginationMeta), AppError> {
    let (page, size, q) = pagination(query)?;
    let filter = "i.wordlist_id=$1 AND ($2='' OR EXISTS(SELECT 1 FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=i.entry_id AND e.archived_at IS NULL AND p.content_schema_version=3 AND strpos(lower(p.snapshot#>>'{presentation,label}'),lower($2))>0))";
    let total = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM wordlist_items i WHERE {filter}"
    )))
    .bind(id)
    .bind(&q)
    .fetch_one(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    let rows=sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT i.entry_id,i.position,i.private_note,i.note_revision FROM wordlist_items i WHERE {filter} ORDER BY i.position LIMIT $3 OFFSET $4"))).bind(id).bind(q).bind(i64::from(size)).bind(i64::from(page-1)*i64::from(size)).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
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
    let filter = "e.archived_at IS NULL AND p.content_schema_version=3 AND strpos(lower(p.snapshot#>>'{presentation,label}'),lower($1))>0";
    let total=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE {filter}"))).bind(&q).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    let entry_ids:Vec<Uuid>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT e.id FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE {filter} ORDER BY (lower(p.snapshot#>>'{{presentation,label}}')=lower($1)) DESC,p.snapshot#>>'{{presentation,label}}',e.id LIMIT $2 OFFSET $3"))).bind(q).bind(i64::from(size)).bind(i64::from(page-1)*i64::from(size)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let mut entries = read_entries(&mut tx, &entry_ids).await?;
    let items = entry_ids
        .iter()
        .filter_map(|id| entries.remove(id))
        .map(projection::candidate)
        .collect();
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistCatalog {
        items,
        pagination: page_meta(page, size, total),
    })
}
