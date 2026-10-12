use super::{dto::*, entry_search, published};
use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiPath, ApiQuery},
    error::{AppError, ErrorCode},
    state::AppState,
};
use axum::{Json, extract::State};
use uuid::Uuid;
#[utoipa::path(get,path="/api/v1/admin/lexicon/entries/related-suggestions",tag="admin-lexicon",security(("bearer_auth"=[])),params(RelatedSuggestionQuery),responses((status=200,description="按词条去重的轻量候选，最多三项",body=RelatedEntrySuggestions),(status=400,description="查询无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=503,description="V3 读取未开启")))]
pub async fn suggest(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<RelatedSuggestionQuery>,
) -> Result<Json<RelatedEntrySuggestions>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "words.access").await?;
    if !state.smart_lexicon_v3_flags.read {
        return Err(super::handler::v3_storage_unavailable());
    }
    let candidates = entry_search::search(&mut tx, &query.q, query.exclude_entry_id).await?;
    let items = candidates
        .into_iter()
        .map(|item| RelatedEntrySuggestion {
            entry_id: item.entry_id,
            publication_id: item.publication_id,
            label: item.label,
            glosses: item.glosses,
            sense_count: item.sense_count,
        })
        .collect();
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(RelatedEntrySuggestions { items }))
}
#[utoipa::path(get,path="/api/v1/admin/lexicon/entries/{id}/relation-senses",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path),RelatedSenseQuery),responses((status=200,description="选中词条的当前发布词义摘要",body=RelatedEntrySenses),(status=400,description="查询无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="词条不可用"),(status=409,description="候选发布版本已变化"),(status=503,description="V3 读取未开启")))]
pub async fn senses(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<RelatedSenseQuery>,
) -> Result<Json<RelatedEntrySenses>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "words.access").await?;
    if !state.smart_lexicon_v3_flags.read {
        return Err(super::handler::v3_storage_unavailable());
    }
    published::lock_entries_in(&mut tx, &[id]).await?;
    let publication_id:Uuid=sqlx::query_scalar("SELECT p.id FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=$1 AND e.archived_at IS NULL AND p.content_schema_version=3").bind(id).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.ok_or_else(||AppError::not_found("关联词已不可用"))?;
    if query
        .publication_id
        .is_some_and(|expected| expected != publication_id)
    {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "关联词已更新，请重新选择",
        ));
    }
    let rows:Vec<(Uuid,String)>=sqlx::query_as("SELECT (sense.value->>'id')::uuid,COALESCE((SELECT definition.value#>>'{content,text}' FROM jsonb_array_elements(sense.value->'definitions') WITH ORDINALITY AS definition(value,n) WHERE definition.value->>'definition_mode' IN ('zh_definition','zh_sentence') ORDER BY definition.n LIMIT 1),'') FROM lexicon.entry_publications p CROSS JOIN LATERAL jsonb_array_elements(p.snapshot#>'{meanings,pos}') WITH ORDINALITY AS pos(value,n) CROSS JOIN LATERAL jsonb_array_elements(pos.value->'senses') WITH ORDINALITY AS sense(value,n) WHERE p.id=$1 ORDER BY pos.n,sense.n").bind(publication_id).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let senses = rows
        .into_iter()
        .map(|(sense_id, gloss)| RelatedWordSenseV3 { sense_id, gloss })
        .collect();
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(RelatedEntrySenses {
        entry_id: id,
        publication_id,
        senses,
    }))
}
