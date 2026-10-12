use super::{
    dto::*,
    review,
    service::{self, Tx},
};
use crate::{
    admin::{AdminAuth, permissions},
    error::{AppError, ErrorCode},
};
use sqlx::PgPool;
use uuid::Uuid;

fn invalid(message: &str) -> AppError {
    AppError::bad_request(ErrorCode::ValidationFailed, message)
}
fn selection() -> String {
    service::META.replace(" FROM wordlists w", ",w.review_state,w.recommended,(SELECT COALESCE(sum(t.amount),0)::text FROM wordlist_tips t WHERE t.wordlist_id=w.id) AS tip_amount FROM wordlists w")
}
async fn meta(tx: &mut Tx<'_>, id: Uuid) -> Result<AdminWordlist, AppError> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{} WHERE w.id=$1",
        selection()
    )))
    .bind(id)
    .fetch_one(&mut **tx)
    .await
    .map_err(AppError::internal)
}
async fn audit(
    tx: &mut Tx<'_>,
    auth: &AdminAuth,
    id: Uuid,
    action: &str,
    revision: i64,
) -> Result<(), AppError> {
    permissions::service::audit(
        tx,
        auth.subject,
        Uuid::now_v7(),
        action,
        "wordlist",
        id,
        serde_json::json!({"revision":revision}),
    )
    .await
}
fn validate(name: &str, items: &[AdminWordlistItemInput]) -> Result<Vec<Uuid>, AppError> {
    let ids: Vec<_> = items.iter().map(|i| i.entry_id).collect();
    service::validate_content(name, &ids)?;
    if items.iter().any(|i| i.admin_note.chars().count() > 1000) {
        return Err(invalid("备注不能超过 1000 字"));
    }
    Ok(ids)
}
pub async fn list(
    pool: &PgPool,
    auth: &AdminAuth,
    query: WordlistManagementQuery,
) -> Result<AdminWordlistPage, AppError> {
    let (page, size, q) = service::pagination(&WordlistQuery {
        q: query.q,
        page: query.page,
        page_size: query.page_size,
    })?;
    if query
        .created_from
        .zip(query.created_before)
        .is_some_and(|(a, b)| a >= b)
    {
        return Err(invalid("创建时间范围无效"));
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    review::lock_admin(&mut tx, auth, "wordlists.access").await?;
    let filter = "w.deleted_at IS NULL AND (strpos(lower(w.name),lower($1))>0 OR strpos(lower(COALESCE(u.display_name,a.display_name)),lower($1))>0) AND ($2::boolean IS NULL OR (w.state='published')=$2) AND ($3::boolean IS NULL OR w.recommended=$3) AND ($4::timestamptz IS NULL OR w.created_at>=$4) AND ($5::timestamptz IS NULL OR w.created_at<$5)";
    let from = "wordlists w LEFT JOIN users u ON u.id=w.owner_user_id LEFT JOIN admins a ON a.id=w.owner_admin_id";
    let total = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {from} WHERE {filter}"
    )))
    .bind(&q)
    .bind(query.public)
    .bind(query.recommended)
    .bind(query.created_from)
    .bind(query.created_before)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let items = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{} WHERE {filter} ORDER BY w.created_at DESC,w.id DESC LIMIT $6 OFFSET $7",
        selection()
    )))
    .bind(q)
    .bind(query.public)
    .bind(query.recommended)
    .bind(query.created_from)
    .bind(query.created_before)
    .bind(i64::from(size))
    .bind(i64::from(page - 1) * i64::from(size))
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(AdminWordlistPage {
        items,
        pagination: service::page_meta(page, size, total),
    })
}
pub async fn detail(pool: &PgPool, auth: &AdminAuth, id: Uuid) -> Result<AdminWordlist, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    review::admin_list_lock(&mut tx, auth, id, "wordlists.access", false).await?;
    let result = meta(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn edit(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
) -> Result<AdminWordlistEdit, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let list = review::admin_list_lock(&mut tx, auth, id, "wordlists.edit", false).await?;
    let rows:Vec<(Uuid,String)>=sqlx::query_as("SELECT entry_id,CASE WHEN $2 THEN admin_note ELSE '' END FROM wordlist_items WHERE wordlist_id=$1 ORDER BY position").bind(id).bind(list.owner_user_id.is_none()).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let mut entries =
        service::read_entries(&mut tx, &rows.iter().map(|(id, _)| *id).collect::<Vec<_>>()).await?;
    let detail = meta(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(AdminWordlistEdit {
        detail,
        items: rows
            .into_iter()
            .map(|(entry_id, admin_note)| AdminWordlistEditItem {
                entry_id,
                admin_note,
                candidate: entries.remove(&entry_id).map(super::projection::candidate),
            })
            .collect(),
    })
}
pub async fn items(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    query: WordlistItemsQuery,
) -> Result<WordlistItems, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let list = review::admin_list_lock(&mut tx, auth, id, "wordlists.access", false).await?;
    let (rows, pagination) = service::item_rows(&mut tx, id, &query).await?;
    let mut entries = service::read_entries_view(
        &mut tx,
        &rows.iter().map(|r| r.entry_id).collect::<Vec<_>>(),
        query.view,
    )
    .await?;
    let items = rows
        .into_iter()
        .map(|r| WordlistItem {
            entry_id: r.entry_id,
            position: r.position,
            entry: entries.remove(&r.entry_id),
        })
        .collect();
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistItems {
        items,
        revision: list.revision,
        pagination,
    })
}
pub async fn catalog(
    pool: &PgPool,
    auth: &AdminAuth,
    query: WordlistQuery,
) -> Result<WordlistCatalog, AppError> {
    let (page, size, q) = service::pagination(&query)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    review::lock_admin(&mut tx, auth, "wordlists.access").await?;
    let result = service::catalog_in(&mut tx, page, size, q).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn create(
    pool: &PgPool,
    auth: &AdminAuth,
    input: CreateAdminWordlist,
) -> Result<AdminWordlist, AppError> {
    let ids = validate(&input.name, &input.items)?;
    let hash = service::hash(&input)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    review::lock_admin(&mut tx, auth, "wordlists.create").await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!(
            "admin-wordlist:{}:{}",
            auth.subject, input.idempotency_key
        ))
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let old: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT id,create_hash FROM wordlists WHERE owner_admin_id=$1 AND create_key=$2",
    )
    .bind(auth.subject)
    .bind(input.idempotency_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let id = if let Some((id, previous)) = old {
        if previous != hash {
            return Err(AppError::conflict(
                ErrorCode::IdempotencyConflict,
                None,
                "创建请求键冲突",
            ));
        }
        service::lock_list(&mut tx, id, None, false).await?;
        id
    } else {
        service::lock_entries(&mut tx, &ids, true).await?;
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO wordlists(id,owner_admin_id,name,create_key,create_hash) VALUES($1,$2,$3,$4,$5)").bind(id).bind(auth.subject).bind(input.name.trim()).bind(input.idempotency_key).bind(hash).execute(&mut *tx).await.map_err(AppError::internal)?;
        write_items(&mut tx, id, &input.items).await?;
        audit(&mut tx, auth, id, "wordlist.create", 1).await?;
        id
    };
    let result = meta(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
async fn write_items(
    tx: &mut Tx<'_>,
    id: Uuid,
    items: &[AdminWordlistItemInput],
) -> Result<(), AppError> {
    let ids: Vec<_> = items.iter().map(|i| i.entry_id).collect();
    let notes: Vec<_> = items.iter().map(|i| i.admin_note.as_str()).collect();
    sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1 AND NOT(entry_id=ANY($2))")
        .bind(id)
        .bind(&ids)
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    // Existing user private notes and their revisions are deliberately untouched.
    sqlx::query("INSERT INTO wordlist_items(wordlist_id,entry_id,position,admin_note) SELECT $1,x.id,(x.n-1)::integer,x.note FROM unnest($2::uuid[],$3::text[]) WITH ORDINALITY AS x(id,note,n) ON CONFLICT(wordlist_id,entry_id) DO UPDATE SET position=excluded.position,admin_note=excluded.admin_note").bind(id).bind(ids).bind(notes).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
pub async fn update(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    input: UpdateAdminWordlist,
) -> Result<AdminWordlist, AppError> {
    let ids = validate(&input.name, &input.items)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let list = review::admin_list_lock(&mut tx, auth, id, "wordlists.edit", true).await?;
    if list.revision != input.expected_revision {
        return Err(service::conflict());
    }
    if list.owner_user_id.is_some() && input.items.iter().any(|i| !i.admin_note.is_empty()) {
        return Err(invalid("用户词表不支持后台备注"));
    }
    service::lock_entries(&mut tx, &ids, true).await?;
    let changed = list.name != input.name.trim() || service::ids(&mut tx, id).await? != ids;
    write_items(&mut tx, id, &input.items).await?;
    if changed {
        sqlx::query("UPDATE wordlist_review_requests SET state='cancelled',decided_at=clock_timestamp() WHERE wordlist_id=$1 AND state='pending'").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    sqlx::query("UPDATE wordlists SET name=$2,state=CASE WHEN $3 THEN 'draft' ELSE state END,review_state=CASE WHEN $3 THEN 'none' ELSE review_state END,revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(input.name.trim()).bind(changed).execute(&mut *tx).await.map_err(AppError::internal)?;
    audit(&mut tx, auth, id, "wordlist.edit", list.revision).await?;
    let result = meta(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn visibility(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    input: WordlistVisibility,
) -> Result<AdminWordlist, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let list = review::admin_list_lock(&mut tx, auth, id, "wordlists.visibility", true).await?;
    if list.revision != input.expected_revision {
        return Err(service::conflict());
    }
    if input.public {
        service::eligible_owner(&mut tx, list.owner_user_id).await?;
        let ids = service::ids(&mut tx, id).await?;
        service::lock_entries(&mut tx, &ids, true).await?;
        approve_entries(&mut tx, id).await?;
    }
    // Review state and pending snapshots are independent of an administrator's visibility override.
    sqlx::query("UPDATE wordlists SET state=$2,revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(if input.public{"published"}else{"draft"}).execute(&mut *tx).await.map_err(AppError::internal)?;
    audit(
        &mut tx,
        auth,
        id,
        if input.public {
            "wordlist.public"
        } else {
            "wordlist.private"
        },
        list.revision,
    )
    .await?;
    if input.public {
        service::eligible_owner(&mut tx, list.owner_user_id).await?;
    }
    let result = meta(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn approve_entries(tx: &mut Tx<'_>, id: Uuid) -> Result<(), AppError> {
    sqlx::query("UPDATE wordlist_items i SET approved_lifecycle_revision=e.lifecycle_revision,approved_archive_generation=e.wordlist_archive_generation FROM lexicon.entries e WHERE i.wordlist_id=$1 AND e.id=i.entry_id").bind(id).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
pub async fn recommend(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    input: WordlistRecommendation,
) -> Result<AdminWordlist, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let list = review::admin_list_lock(&mut tx, auth, id, "wordlists.recommend", true).await?;
    if list.revision != input.expected_revision
        || (input.recommended && list.state != WordlistState::Published)
    {
        return Err(service::conflict());
    }
    sqlx::query("UPDATE wordlists SET recommended=$2,revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(input.recommended).execute(&mut *tx).await.map_err(AppError::internal)?;
    audit(&mut tx, auth, id, "wordlist.recommend", list.revision).await?;
    let result = meta(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn delete(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    input: WithdrawWordlist,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let list = review::admin_list_lock(&mut tx, auth, id, "wordlists.delete", true).await?;
    if list.revision != input.expected_revision {
        return Err(service::conflict());
    }
    sqlx::query("UPDATE wordlist_review_requests SET state='cancelled',decided_at=clock_timestamp() WHERE wordlist_id=$1 AND state='pending'").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    sqlx::query("UPDATE wordlists SET state='draft',deleted_at=clock_timestamp(),revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    audit(&mut tx, auth, id, "wordlist.delete", list.revision).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(())
}
pub async fn recommended(pool: &PgPool, query: WordlistQuery) -> Result<WordlistPage, AppError> {
    let (page, size, q) = service::pagination(&query)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let filter = format!(
        "w.recommended AND w.state='published' AND w.deleted_at IS NULL AND {} AND strpos(lower(w.name),lower($1))>0",
        service::VISIBLE_OWNER
    );
    let total = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM wordlists w LEFT JOIN users u ON u.id=w.owner_user_id WHERE {filter}"
    )))
    .bind(&q)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let items = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{} WHERE {filter} ORDER BY w.created_at DESC,w.id DESC LIMIT $2 OFFSET $3",
        service::META
    )))
    .bind(q)
    .bind(i64::from(size))
    .bind(i64::from(page - 1) * i64::from(size))
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistPage {
        items,
        pagination: service::page_meta(page, size, total),
    })
}
