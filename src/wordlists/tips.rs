use super::{
    dto::{WordlistQuery, WordlistState},
    service,
};
use crate::{
    api::{ApiJson, ApiPath, ApiQuery, PaginationMeta},
    auth::extract::AuthUser,
    coins::{
        self,
        model::{Actor, Amount, CoinError, Context, Owner, OwnerType},
    },
    error::{AppError, ErrorCode},
    state::AppState,
};
use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use coins::admin_service::coin_error;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWordlistTip {
    pub event_id: Uuid,
    pub idempotency_key: Uuid,
    #[schema(pattern = "^[1-9][0-9]{0,18}$")]
    pub amount: String,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct WordlistTip {
    pub event_id: Uuid,
    pub wordlist_id: Uuid,
    pub payer_user_id: Uuid,
    pub author_user_id: Uuid,
    #[schema(pattern = "^[1-9][0-9]{0,18}$")]
    pub amount: String,
    pub operation_id: Uuid,
    pub created_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct WordlistTipRecord {
    pub event_id: Uuid,
    pub wordlist_id: Uuid,
    pub payer_user_id: Uuid,
    pub author_user_id: Uuid,
    #[schema(pattern = "^[1-9][0-9]{0,18}$")]
    pub amount: String,
    pub operation_id: Uuid,
    pub created_at: DateTime<Utc>,
    #[schema(required = true)]
    pub wordlist_name: Option<String>,
    pub wordlist_accessible: bool,
    #[schema(required = true)]
    pub counterparty_name: Option<String>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistTipPage {
    pub items: Vec<WordlistTipRecord>,
    pub pagination: PaginationMeta,
}
const TIP: &str = "SELECT event_id,wordlist_id,payer_user_id,author_user_id,amount::text AS amount,operation_id,created_at FROM wordlist_tips";
async fn available_wallets(tx: &mut service::Tx<'_>, users: &[Uuid]) -> Result<(), AppError> {
    // User locks already serialize lifecycle mutations. Do not take early wallet locks.
    let unavailable:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM coin_wallets WHERE owner_type='user' AND owner_id=ANY($1) AND status<>'open') OR EXISTS(SELECT 1 FROM account_deletion_requests WHERE user_id=ANY($1) AND status='pending')").bind(users).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
    if unavailable {
        return Err(coin_error(CoinError::WalletUnavailable));
    }
    Ok(())
}
pub async fn transfer(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: CreateWordlistTip,
) -> Result<WordlistTip, AppError> {
    let value = input
        .amount
        .parse::<i64>()
        .map_err(|_| coin_error(CoinError::InvalidAmount))?;
    if value.to_string() != input.amount {
        return Err(coin_error(CoinError::InvalidAmount));
    }
    let amount = Amount::new(value).map_err(coin_error)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let author: Uuid =
        sqlx::query_scalar("SELECT owner_user_id FROM wordlists WHERE id=$1 AND state='published'")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(AppError::internal)?
            .ok_or_else(|| AppError::not_found("wordlist not found"))?;
    if author == auth.subject {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "不能给自己的词表投币",
        ));
    }
    let payer = Owner {
        owner_type: OwnerType::User,
        owner_id: auth.subject,
    };
    let payee = Owner {
        owner_type: OwnerType::User,
        owner_id: author,
    };
    coins::service::lock_accounts_in(&mut tx, &[payer, payee])
        .await
        .map_err(coin_error)?;
    service::lock_user(&mut tx, auth).await?;
    available_wallets(&mut tx, &[auth.subject, author]).await?;
    let list = service::lock_list(&mut tx, id, None, true).await?;
    if list.state != WordlistState::Published {
        return Err(AppError::not_found("wordlist not found"));
    }
    let ids = service::ids(&mut tx, id).await?;
    service::lock_entries(&mut tx, &ids, true).await?;
    let requires_review:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wordlist_items i JOIN lexicon.entries e ON e.id=i.entry_id WHERE i.wordlist_id=$1 AND i.approved_archive_generation IS DISTINCT FROM e.wordlist_archive_generation)").bind(id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    if requires_review {
        return Err(AppError::conflict(
            ErrorCode::WordListConflict,
            None,
            "词条曾不可用，作者修复并重新审核后才能投币",
        ));
    }
    let hash = service::hash(&(auth.subject, id, author, &input))?;
    for key in [
        format!(
            "wordlist-tip-request:{}:{}",
            auth.subject, input.idempotency_key
        ),
        format!("wordlist-tip-event:{}", input.event_id),
    ] {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(key)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    }
    let old: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT event_id,request_hash FROM wordlist_tips WHERE payer_user_id=$1 AND request_key=$2",
    )
    .bind(auth.subject)
    .bind(input.idempotency_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    if let Some((event, old_hash)) = old {
        if old_hash != hash {
            return Err(coin_error(CoinError::IdempotencyConflict));
        }
        let result = sqlx::query_as(sqlx::AssertSqlSafe(format!("{TIP} WHERE event_id=$1")))
            .bind(event)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
        crate::account_deletion::ensure_not_effective_in(&mut tx, author).await?;
        tx.commit().await.map_err(AppError::internal)?;
        return Ok(result);
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wordlist_tips WHERE event_id=$1)")
            .bind(input.event_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    if exists {
        return Err(coin_error(CoinError::SourceConflict));
    }
    let ctx = Context {
        actor: Actor::Account(payer),
        idempotency_scope: format!("wordlist-tip:{}", auth.subject),
        idempotency_key: input.idempotency_key.to_string(),
        source_type: "wordlist_tip".into(),
        source_id: input.event_id.to_string(),
        reason: "词表投币".into(),
        evidence_ref: None,
    };
    let receipt = coins::service::transfer_in(&mut tx, payer, payee, amount, &ctx)
        .await
        .map_err(coin_error)?;
    sqlx::query("INSERT INTO wordlist_tips(event_id,wordlist_id,payer_user_id,author_user_id,amount,request_key,request_hash,operation_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(input.event_id).bind(id).bind(auth.subject).bind(author).bind(value).bind(input.idempotency_key).bind(hash).bind(receipt.operation_id).execute(&mut *tx).await.map_err(AppError::internal)?;
    let result = sqlx::query_as(sqlx::AssertSqlSafe(format!("{TIP} WHERE event_id=$1")))
        .bind(input.event_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, author).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn records(
    pool: &PgPool,
    auth: &AuthUser,
    query: WordlistQuery,
) -> Result<WordlistTipPage, AppError> {
    let (page, size, _) = service::pagination(&query)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::lock_user(&mut tx, auth).await?;
    let total = sqlx::query_scalar(
        "SELECT count(*) FROM wordlist_tips WHERE payer_user_id=$1 OR author_user_id=$1",
    )
    .bind(auth.subject)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    // A payer must not learn a renamed private wordlist after its author withdraws it.
    let items=sqlx::query_as("SELECT t.event_id,t.wordlist_id,t.payer_user_id,t.author_user_id,t.amount::text AS amount,t.operation_id,t.created_at,CASE WHEN w.owner_user_id=$1 OR (w.state='published' AND author.status='active' AND NOT EXISTS(SELECT 1 FROM account_deletion_requests d WHERE d.user_id=author.id AND d.status='pending' AND d.effective_at<=clock_timestamp())) THEN w.name ELSE NULL END AS wordlist_name,COALESCE(w.owner_user_id=$1 OR (w.state='published' AND author.status='active' AND NOT EXISTS(SELECT 1 FROM account_deletion_requests d WHERE d.user_id=author.id AND d.status='pending' AND d.effective_at<=clock_timestamp())),false) AS wordlist_accessible,counterparty.display_name AS counterparty_name FROM wordlist_tips t LEFT JOIN wordlists w ON w.id=t.wordlist_id LEFT JOIN users author ON author.id=t.author_user_id LEFT JOIN users counterparty ON counterparty.id=CASE WHEN t.payer_user_id=$1 THEN t.author_user_id ELSE t.payer_user_id END WHERE t.payer_user_id=$1 OR t.author_user_id=$1 ORDER BY t.created_at DESC,t.event_id DESC LIMIT $2 OFFSET $3").bind(auth.subject).bind(i64::from(size)).bind(i64::from(page-1)*i64::from(size)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistTipPage {
        items,
        pagination: service::page_meta(page, size, total),
    })
}
#[utoipa::path(post,path="/api/v1/wordlists/{id}/tips",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),request_body=CreateWordlistTip,
responses((status=200,description="独立投币事件与双边转账原子提交",body=WordlistTip),(status=400,description="数量或词条不可用"),(status=401,description="会话无效"),(status=403,description="需要先绑定手机号；禁止自投"),(status=404,description="词表不可访问"),(status=409,description="余额、钱包、幂等或重审冲突")))]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<CreateWordlistTip>,
) -> Result<Json<WordlistTip>, AppError> {
    Ok(Json(transfer(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(get,path="/api/v1/me/wordlist-tips",tag="wordlists",params(WordlistQuery),security(("bearer_auth"=[])),responses((status=403,description="需要先绑定手机号"),(status=200,description="本人投出与收到记录，不泄露私密词表新名称",body=WordlistTipPage),(status=400,description="查询无效"),(status=401,description="会话无效")))]
pub async fn history(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Json<WordlistTipPage>, AppError> {
    Ok(Json(records(&state.pool, &auth, query).await?))
}

#[utoipa::path(get,path="/api/v1/me/wordlist-tips/{event_id}",tag="wordlists",params(("event_id"=Uuid,Path)),security(("bearer_auth"=[])),responses((status=403,description="需要先绑定手机号"),(status=200,description="核对本人已完成事件；不依赖词表仍公开或存在",body=WordlistTip),(status=401,description="会话无效"),(status=404,description="事件不存在或不属于本人")))]
pub async fn receipt(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(event_id): ApiPath<Uuid>,
) -> Result<Json<WordlistTip>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    service::lock_user(&mut tx, &auth).await?;
    let result = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{TIP} WHERE event_id=$1 AND (payer_user_id=$2 OR author_user_id=$2)"
    )))
    .bind(event_id)
    .bind(auth.subject)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("tip not found"))?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(result))
}
