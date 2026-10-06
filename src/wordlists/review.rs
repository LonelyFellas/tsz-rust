use super::{
    dto::*,
    service::{self, Tx},
};
use crate::{
    admin::{AdminAuth, permissions},
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
};
use sqlx::PgPool;
use uuid::Uuid;
const REVIEW: &str = "SELECT id,wordlist_id,submitted_revision,name,state,reason,created_at,decided_at FROM wordlist_review_requests";
async fn review(tx: &mut Tx<'_>, id: Uuid, request: Uuid) -> Result<WordlistReview, AppError> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{REVIEW} WHERE wordlist_id=$1 AND id=$2"
    )))
    .bind(id)
    .bind(request)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("review not found"))
}
pub async fn submit(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: SubmitWordlist,
) -> Result<WordlistReview, AppError> {
    let hash = service::hash(&input)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::lock_user(&mut tx, auth).await?;
    let list = service::lock_list(&mut tx, id, Some(auth.subject), true).await?;
    let old:Option<(Uuid,Vec<u8>)>=sqlx::query_as("SELECT id,request_hash FROM wordlist_review_requests WHERE wordlist_id=$1 AND submit_key=$2").bind(id).bind(input.idempotency_key).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    let request = if let Some((request, old_hash)) = old {
        if old_hash != hash {
            return Err(AppError::conflict(
                ErrorCode::IdempotencyConflict,
                None,
                "提交请求键冲突",
            ));
        }
        request
    } else {
        if list.revision != input.expected_revision
            || matches!(
                list.state,
                WordlistState::Pending | WordlistState::Published
            )
        {
            return Err(service::conflict());
        }
        let ids = service::ids(&mut tx, id).await?;
        service::lock_entries(&mut tx, &ids, true).await?;
        let request = Uuid::now_v7();
        sqlx::query("UPDATE wordlists SET state='pending',revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("INSERT INTO wordlist_review_requests(id,wordlist_id,submitted_revision,name,entry_ids,submit_key,request_hash) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(request).bind(id).bind(list.revision+1).bind(list.name).bind(ids).bind(input.idempotency_key).bind(hash).execute(&mut *tx).await.map_err(AppError::internal)?;
        request
    };
    let result = review(&mut tx, id, request).await?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn withdraw(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: WithdrawWordlist,
) -> Result<Wordlist, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::lock_user(&mut tx, auth).await?;
    let list = service::lock_list(&mut tx, id, Some(auth.subject), true).await?;
    if list.revision != input.expected_revision
        || !matches!(
            list.state,
            WordlistState::Published | WordlistState::Pending
        )
    {
        return Err(service::conflict());
    }
    sqlx::query("UPDATE wordlist_review_requests SET state='cancelled',decided_at=clock_timestamp() WHERE wordlist_id=$1 AND state='pending'").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    sqlx::query("UPDATE wordlists SET state='draft',revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    let result = service::meta(&mut tx, id).await?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn lock_admin(
    tx: &mut Tx<'_>,
    auth: &AdminAuth,
    permission: &str,
) -> Result<(), AppError> {
    let version: Option<i64> =
        sqlx::query_scalar("SELECT security_version FROM admins WHERE id=$1 FOR SHARE")
            .bind(auth.subject)
            .fetch_optional(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    if version != Some(auth.security_version) {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    permissions::lock(tx, auth.subject)
        .await?
        .require(permission)
}
// Owner::Ord is user then admin; lock the user before any admin/permission/list lock.
async fn admin_list_lock(
    tx: &mut Tx<'_>,
    auth: &AdminAuth,
    id: Uuid,
    permission: &str,
    write: bool,
) -> Result<Wordlist, AppError> {
    let owner: Uuid = sqlx::query_scalar("SELECT owner_user_id FROM wordlists WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("wordlist not found"))?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR SHARE")
        .bind(owner)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    lock_admin(tx, auth, permission).await?;
    service::lock_list(tx, id, None, write).await
}
fn reason(value: &str) -> Result<&str, AppError> {
    let v = value.trim();
    if v.is_empty() || v.chars().count() > 1000 {
        return Err(AppError::bad_request(
            ErrorCode::ValidationFailed,
            "原因须为 1–1000 字",
        ));
    }
    Ok(v)
}
pub async fn decision(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    request: Uuid,
    input: WordlistDecision,
) -> Result<Wordlist, AppError> {
    let explanation = if input.approve {
        None
    } else {
        Some(reason(input.reason.as_deref().unwrap_or(""))?)
    };
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let list = admin_list_lock(&mut tx, auth, id, "wordlists.review", true).await?;
    let pending = review(&mut tx, id, request).await?;
    if list.state != WordlistState::Pending
        || list.revision != input.expected_revision
        || pending.state != "pending"
        || pending.submitted_revision != list.revision
    {
        return Err(service::conflict());
    }
    if input.approve {
        service::eligible_owner(&mut tx, list.owner_user_id).await?;
        let ids = service::ids(&mut tx, id).await?;
        service::lock_entries(&mut tx, &ids, true).await?;
        sqlx::query("UPDATE wordlist_items i SET approved_lifecycle_revision=e.lifecycle_revision,approved_archive_generation=e.wordlist_archive_generation FROM lexicon.entries e WHERE i.wordlist_id=$1 AND e.id=i.entry_id").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    sqlx::query("UPDATE wordlists SET state=$2,revision=revision+1,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(if input.approve{"published"}else{"rejected"}).execute(&mut *tx).await.map_err(AppError::internal)?;
    sqlx::query("UPDATE wordlist_review_requests SET state=$2,reviewer_id=$3,reason=$4,decided_at=clock_timestamp() WHERE id=$1").bind(request).bind(if input.approve{"approved"}else{"rejected"}).bind(auth.subject).bind(explanation).execute(&mut *tx).await.map_err(AppError::internal)?;
    permissions::service::audit(&mut tx,auth.subject,Uuid::now_v7(),if input.approve{"wordlist.approve"}else{"wordlist.reject"},"wordlist",id,serde_json::json!({"review_request_id":request,"submitted_revision":pending.submitted_revision})).await?;
    let result = service::meta(&mut tx, id).await?;
    if input.approve {
        service::eligible_owner(&mut tx, list.owner_user_id).await?;
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn admin_withdraw(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    input: AdminWithdrawWordlist,
) -> Result<Wordlist, AppError> {
    let explanation = reason(&input.reason)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let list = admin_list_lock(&mut tx, auth, id, "wordlists.withdraw", true).await?;
    if list.state != WordlistState::Published || list.revision != input.expected_revision {
        return Err(service::conflict());
    }
    sqlx::query("UPDATE wordlists SET state='withdrawn',revision=revision+1,updated_at=clock_timestamp(),withdraw_reason=$2 WHERE id=$1").bind(id).bind(explanation).execute(&mut *tx).await.map_err(AppError::internal)?;
    permissions::service::audit(
        &mut tx,
        auth.subject,
        Uuid::now_v7(),
        "wordlist.withdraw",
        "wordlist",
        id,
        serde_json::json!({"revision":list.revision}),
    )
    .await?;
    let result = service::meta(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn reviews(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
) -> Result<WordlistReviews, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::read_lock(&mut tx, id, Some(auth)).await?;
    let items = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{REVIEW} WHERE wordlist_id=$1 ORDER BY created_at DESC,id DESC LIMIT 20"
    )))
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    let withdraw_reason=sqlx::query_scalar("SELECT CASE WHEN state='withdrawn' THEN withdraw_reason ELSE NULL END FROM wordlists WHERE id=$1").bind(id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistReviews {
        items,
        withdraw_reason,
    })
}
pub async fn admin_reviews(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
) -> Result<WordlistReviews, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    admin_list_lock(&mut tx, auth, id, "wordlists.access", false).await?;
    let items = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{REVIEW} WHERE wordlist_id=$1 ORDER BY created_at DESC,id DESC LIMIT 20"
    )))
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let withdraw_reason=sqlx::query_scalar("SELECT CASE WHEN state='withdrawn' THEN withdraw_reason ELSE NULL END FROM wordlists WHERE id=$1").bind(id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistReviews {
        items,
        withdraw_reason,
    })
}
pub async fn admin_items(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    request: Uuid,
    query: WordlistQuery,
) -> Result<WordlistItems, AppError> {
    let (page, size, _) = service::pagination(&query)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    admin_list_lock(&mut tx, auth, id, "wordlists.access", false).await?;
    let review = review(&mut tx, id, request).await?;
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT entry_ids FROM wordlist_review_requests WHERE id=$1")
            .bind(request)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let selected: Vec<_> = ids
        .iter()
        .enumerate()
        .skip((page as usize - 1) * size as usize)
        .take(size as usize)
        .collect();
    let mut entries = service::read_entries(
        &mut tx,
        &selected.iter().map(|(_, id)| **id).collect::<Vec<_>>(),
    )
    .await?;
    let items = selected
        .into_iter()
        .map(|(pos, id)| WordlistItem {
            entry_id: *id,
            position: pos as i32,
            entry: entries.remove(id),
        })
        .collect();
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistItems {
        items,
        revision: review.submitted_revision,
        pagination: service::page_meta(page, size, ids.len() as i64),
    })
}
pub async fn admin_list(
    pool: &PgPool,
    auth: &AdminAuth,
    query: AdminWordlistQuery,
) -> Result<WordlistPage, AppError> {
    let (page, size, q) = service::pagination(&WordlistQuery {
        q: query.q,
        page: query.page,
        page_size: query.page_size,
    })?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_admin(&mut tx, auth, "wordlists.access").await?;
    let total =
        sqlx::query_scalar("SELECT count(*) FROM wordlists WHERE state<>'draft' AND strpos(lower(name),lower($1))>0 AND ($2::text IS NULL OR state=$2)")
            .bind(&q).bind(&query.state)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let items=sqlx::query_as("SELECT w.id,w.owner_user_id,u.display_name AS owner_name,w.name,w.state,w.revision,(SELECT count(*) FROM wordlist_items i WHERE i.wordlist_id=w.id) AS item_count,w.created_at,w.updated_at FROM wordlists w JOIN users u ON u.id=w.owner_user_id WHERE w.state<>'draft' AND strpos(lower(w.name),lower($1))>0 AND ($2::text IS NULL OR w.state=$2) ORDER BY (w.state='pending') DESC,w.updated_at DESC,w.id DESC LIMIT $3 OFFSET $4").bind(q).bind(&query.state).bind(i64::from(size)).bind(i64::from(page-1)*i64::from(size)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistPage {
        items,
        pagination: service::page_meta(page, size, total),
    })
}
