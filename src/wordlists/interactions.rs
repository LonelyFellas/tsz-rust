//! Reference-based interactions. Existing wordlist responses remain wire-compatible.
use super::{
    dto::{Wordlist, WordlistQuery},
    private_json, service,
};
use crate::{
    api::{ApiJson, ApiPath, ApiQuery, PaginationMeta},
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
    state::AppState,
};
use axum::{Json, extract::State, response::Response};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct InteractionQuery {
    /// Comma-separated wordlist UUIDs, at most 100.
    pub ids: String,
}
impl InteractionQuery {
    pub fn parse(&self) -> Result<Vec<Uuid>, AppError> {
        let ids: Result<Vec<_>, _> = self.ids.split(',').map(str::parse::<Uuid>).collect();
        match ids {
            Ok(ids) if !ids.is_empty() && ids.len() <= 100 => Ok(ids),
            _ => Err(AppError::bad_request(
                ErrorCode::InvalidQuery,
                "需提供 1–100 个词表 ID",
            )),
        }
    }
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct WordlistInteractionStats {
    pub wordlist_id: Uuid,
    #[schema(minimum = 0)]
    pub like_count: i64,
    #[schema(minimum = 0)]
    pub favorite_count: i64,
    #[schema(pattern = "^(0|[1-9][0-9]*)$")]
    pub tip_amount: String,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistInteractionStatsBatch {
    pub items: Vec<WordlistInteractionStats>,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct WordlistInteractionState {
    pub wordlist_id: Uuid,
    pub liked: bool,
    pub favorited: bool,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistInteractionStates {
    pub items: Vec<WordlistInteractionState>,
}
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WordlistInteractionKind {
    Like,
    Favorite,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetWordlistInteraction {
    pub folder_id: Option<Uuid>,
    pub kind: WordlistInteractionKind,
    pub active: bool,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistFavorite {
    pub wordlist_id: Uuid,
    /// Null while the original is not publicly accessible. No historical metadata is exposed.
    #[schema(required = true)]
    pub wordlist: Option<Wordlist>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistFavoritePage {
    pub items: Vec<WordlistFavorite>,
    pub pagination: PaginationMeta,
}
#[utoipa::path(get,path="/api/v1/wordlists/interactions",tag="wordlists",params(InteractionQuery),responses((status=200,description="公开词表真实统计；不可用词表不返回",body=WordlistInteractionStatsBatch),(status=400,description="查询无效")))]
pub async fn stats(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<InteractionQuery>,
) -> Result<Json<WordlistInteractionStatsBatch>, AppError> {
    let ids = query.parse()?;
    let items = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT w.id AS wordlist_id,(SELECT count(*) FROM wordlist_interactions i WHERE i.wordlist_id=w.id AND i.kind='like') AS like_count,(SELECT count(*) FROM wordlist_interactions i WHERE i.wordlist_id=w.id AND i.kind='favorite') AS favorite_count,(SELECT COALESCE(sum(t.amount),0)::text FROM wordlist_tips t WHERE t.wordlist_id=w.id) AS tip_amount FROM wordlists w LEFT JOIN users u ON u.id=w.owner_user_id WHERE w.id=ANY($1) AND w.deleted_at IS NULL AND w.state='published' AND {} ORDER BY w.id",service::VISIBLE_OWNER)))
        .bind(ids).fetch_all(&state.pool).await.map_err(AppError::internal)?;
    Ok(Json(WordlistInteractionStatsBatch { items }))
}
#[utoipa::path(get,path="/api/v1/me/wordlist-interactions",tag="wordlists",params(InteractionQuery),security(("bearer_auth"=[])),responses((status=200,description="本人互动状态，不缓存",body=WordlistInteractionStates),(status=400,description="查询无效"),(status=401,description="未登录"),(status=403,description="账户不可用")))]
pub async fn states(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<InteractionQuery>,
) -> Result<Response, AppError> {
    let ids = query.parse()?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    service::lock_user(&mut tx, &auth).await?;
    let items = sqlx::query_as("SELECT x.id AS wordlist_id, EXISTS(SELECT 1 FROM wordlist_interactions i WHERE i.wordlist_id=x.id AND i.user_id=$2 AND i.kind='like') AS liked, EXISTS(SELECT 1 FROM wordlist_interactions i WHERE i.wordlist_id=x.id AND i.user_id=$2 AND i.kind='favorite') AS favorited FROM (SELECT DISTINCT unnest($1::uuid[]) AS id) x ORDER BY x.id")
        .bind(ids).bind(auth.subject).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(private_json(WordlistInteractionStates { items }))
}
#[utoipa::path(put,path="/api/v1/me/wordlists/{id}/interactions",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),request_body=SetWordlistInteraction,responses((status=200,description="设置最终状态，重复请求不重复计数",body=WordlistInteractionState),(status=400,description="参数无效"),(status=401,description="未登录"),(status=403,description="不能操作自己的词表"),(status=404,description="词表不可用")))]
pub async fn set(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<SetWordlistInteraction>,
) -> Result<Response, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    service::lock_user(&mut tx, &auth).await?;
    super::library::lock(&mut tx, auth.subject).await?;
    // Serialize even opposite intents from the same account; additions also lock public visibility.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("wordlist-interaction:{}:{id}", auth.subject))
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let kind = match input.kind {
        WordlistInteractionKind::Like => "like",
        WordlistInteractionKind::Favorite => "favorite",
    };
    if input.active {
        let list = service::read_lock(&mut tx, id, None).await?;
        if list.owner_user_id == Some(auth.subject) {
            return Err(AppError::forbidden(
                ErrorCode::Forbidden,
                "不能给自己的词表点赞或收藏",
            ));
        }
        let inserted = sqlx::query("INSERT INTO wordlist_interactions(wordlist_id,user_id,kind) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(id).bind(auth.subject).bind(kind).execute(&mut *tx).await.map_err(AppError::internal)?;
        if kind == "favorite" {
            super::library::add_wordlist(
                &mut tx,
                auth.subject,
                super::library::dto::LibrarySpace::Favorites,
                id,
                input.folder_id,
            )
            .await?;
        }
        service::eligible_owner(&mut tx, list.owner_user_id).await?;
        if kind == "like"
            && inserted.rows_affected() == 1
            && let Some(author) = list.owner_user_id
        {
            super::notifications::notify(&mut tx, author, auth.subject, id, None).await?;
        }
    } else {
        if kind == "favorite" {
            super::library::remove_favorite(&mut tx, auth.subject, id).await?;
        }
        // Removal must remain possible after withdrawal, account suspension or deletion.
        sqlx::query(
            "DELETE FROM wordlist_interactions WHERE wordlist_id=$1 AND user_id=$2 AND kind=$3",
        )
        .bind(id)
        .bind(auth.subject)
        .bind(kind)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    }
    let result: WordlistInteractionState = sqlx::query_as("SELECT $1::uuid AS wordlist_id, EXISTS(SELECT 1 FROM wordlist_interactions WHERE wordlist_id=$1 AND user_id=$2 AND kind='like') AS liked, EXISTS(SELECT 1 FROM wordlist_interactions WHERE wordlist_id=$1 AND user_id=$2 AND kind='favorite') AS favorited").bind(id).bind(auth.subject).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(private_json(result))
}
#[utoipa::path(get,path="/api/v1/me/wordlist-favorites",tag="wordlists",params(WordlistQuery),security(("bearer_auth"=[])),responses((status=200,description="引用收藏分页；不可用时不返回词表内容",body=WordlistFavoritePage),(status=400,description="查询无效"),(status=401,description="未登录"),(status=403,description="账户不可用")))]
pub async fn favorites(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Response, AppError> {
    let (page, size, q) = service::pagination(&query)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    // One snapshot for pagination and accessible metadata prevents withdrawal leaking through a second read.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    service::lock_user(&mut tx, &auth).await?;
    let filter = format!(
        "i.user_id=$1 AND i.kind='favorite' AND ($2='' OR (w.deleted_at IS NULL AND w.state='published' AND {} AND strpos(lower(w.name),lower($2))>0))",
        service::VISIBLE_OWNER
    );
    let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM wordlist_interactions i JOIN wordlists w ON w.id=i.wordlist_id LEFT JOIN users u ON u.id=w.owner_user_id WHERE {filter}"))).bind(auth.subject).bind(&q).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    let ids: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT w.id FROM wordlist_interactions i JOIN wordlists w ON w.id=i.wordlist_id LEFT JOIN users u ON u.id=w.owner_user_id WHERE {filter} ORDER BY i.created_at DESC,w.id LIMIT $3 OFFSET $4"))).bind(auth.subject).bind(&q).bind(i64::from(size)).bind(i64::from(page-1)*i64::from(size)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let lists: Vec<Wordlist> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{} WHERE w.id=ANY($1) AND w.deleted_at IS NULL AND w.state='published' AND {}",
        service::META,
        service::VISIBLE_OWNER
    )))
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let mut lists: std::collections::HashMap<_, _> = lists.into_iter().map(|w| (w.id, w)).collect();
    let items = ids
        .into_iter()
        .map(|wordlist_id| WordlistFavorite {
            wordlist_id,
            wordlist: lists.remove(&wordlist_id),
        })
        .collect();
    tx.commit().await.map_err(AppError::internal)?;
    Ok(private_json(WordlistFavoritePage {
        items,
        pagination: service::page_meta(page, size, total),
    }))
}
