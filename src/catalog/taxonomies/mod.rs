pub mod model;

use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiJson, ApiPath, ApiQuery, PaginatedResponse, page_meta},
    catalog::model::DeleteRevisionQuery,
    error::{AppError, ErrorCode},
    state::AppState,
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, patch},
};
use model::*;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{kind}", get(list).post(create))
        .route("/{kind}/{id}", patch(update).delete(remove))
}

fn database_error(error: sqlx::Error) -> AppError {
    if let Some(db) = error
        .as_database_error()
        .filter(|db| db.is_unique_violation())
    {
        let field = match db.constraint() {
            Some("lexicon_taxonomies_code_key") => "code",
            Some("lexicon_taxonomies_name_zh_key") => "name_zh",
            Some("lexicon_taxonomies_name_en_key") => "name_en",
            _ => return AppError::internal(error),
        };
        return AppError::conflict(
            ErrorCode::TaxonomyConflict,
            Some(field),
            "taxonomy value already exists",
        );
    }
    AppError::internal(error)
}

#[utoipa::path(get, path = "/api/v1/admin/lexicon/taxonomies/{kind}", tag = "admin-catalog",
    security(("bearer_auth" = [])), params(TaxonomyPath, TaxonomyListQuery),
    responses((status = 200, body = PaginatedResponse<LexiconTaxonomy>),
        (status = 400, description = "查询或分类无效"), (status = 401, description = "未登录"),
        (status = 403, description = "需要词库配置查看权限"), (status = 500, description = "数据库错误")))]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<TaxonomyPath>,
    ApiQuery(query): ApiQuery<TaxonomyListQuery>,
) -> Result<Json<PaginatedResponse<LexiconTaxonomy>>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("lexicon_settings.access")?;
    let page = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(20);
    if page == 0
        || !(1..=100).contains(&size)
        || query.q.as_ref().is_some_and(|q| q.chars().count() > 100)
    {
        return Err(AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "invalid pagination or search",
        ));
    }
    let q = query.q.as_deref().unwrap_or("").trim();
    let total = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catalog.lexicon_taxonomies WHERE kind=$1 AND (strpos(lower(code),lower($2))>0 OR strpos(lower(name_zh),lower($2))>0 OR strpos(lower(name_en),lower($2))>0)")
        .bind(path.kind.as_str()).bind(q).fetch_one(&state.pool).await.map_err(AppError::internal)?;
    let items = sqlx::query_as::<_, LexiconTaxonomy>("SELECT id,code,name_zh,name_en,sort_order,revision,created_at,updated_at FROM catalog.lexicon_taxonomies WHERE kind=$1 AND (strpos(lower(code),lower($2))>0 OR strpos(lower(name_zh),lower($2))>0 OR strpos(lower(name_en),lower($2))>0) ORDER BY sort_order,created_at,id LIMIT $3 OFFSET $4")
        .bind(path.kind.as_str()).bind(q).bind(i64::from(size)).bind((i64::from(page)-1)*i64::from(size))
        .fetch_all(&state.pool).await.map_err(AppError::internal)?;
    Ok(Json(PaginatedResponse {
        items,
        pagination: page_meta(page, size, total),
    }))
}

#[utoipa::path(post, path = "/api/v1/admin/lexicon/taxonomies/{kind}", tag = "admin-catalog",
    security(("bearer_auth" = [])), params(TaxonomyPath), request_body = CreateTaxonomyRequest,
    responses((status = 201, body = LexiconTaxonomy), (status = 400, description = "字段或分类无效"),
        (status = 401, description = "未登录"), (status = 403, description = "需要目录编辑权限"),
        (status = 409, description = "同类别的编码或名称重复"), (status = 422, description = "请求结构无效"),
        (status = 500, description = "数据库错误")))]
pub async fn create(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<TaxonomyPath>,
    ApiJson(input): ApiJson<CreateTaxonomyRequest>,
) -> Result<(StatusCode, Json<LexiconTaxonomy>), AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("lexicon_settings.edit_taxonomies")?;
    let valid_code = !input.code.is_empty()
        && input.code.len() <= 32
        && input.code.as_bytes()[0].is_ascii_lowercase()
        && input
            .code
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_');
    if !valid_code {
        return Err(AppError::validation(
            ErrorCode::InvalidRequestBody,
            "code",
            "invalid taxonomy code",
        ));
    }
    let zh = text(input.name_zh, "name_zh")?;
    let en = text(input.name_en, "name_en")?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "lexicon_settings.edit_taxonomies").await?;
    let item = sqlx::query_as::<_, LexiconTaxonomy>("INSERT INTO catalog.lexicon_taxonomies (id,kind,code,name_zh,name_en,sort_order,created_by_admin_id,updated_by_admin_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$7) RETURNING id,code,name_zh,name_en,sort_order,revision,created_at,updated_at")
        .bind(Uuid::now_v7()).bind(path.kind.as_str()).bind(input.code).bind(zh).bind(en).bind(input.sort_order).bind(auth.subject)
        .fetch_one(&mut *tx).await.map_err(database_error)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok((StatusCode::CREATED, Json(item)))
}

async fn lock_item(
    tx: &mut Transaction<'_, Postgres>,
    path: &TaxonomyItemPath,
    base_revision: i64,
) -> Result<(), AppError> {
    revision(base_revision)?;
    let current = sqlx::query_scalar::<_, i64>(
        "SELECT revision FROM catalog.lexicon_taxonomies WHERE kind=$1 AND id=$2 FOR UPDATE",
    )
    .bind(path.kind.as_str())
    .bind(path.id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("taxonomy not found"))?;
    if current != base_revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("base_revision"),
            "taxonomy changed; reload before retrying",
        ));
    }
    Ok(())
}

#[utoipa::path(patch, path = "/api/v1/admin/lexicon/taxonomies/{kind}/{id}", tag = "admin-catalog",
    security(("bearer_auth" = [])), params(TaxonomyItemPath), request_body = UpdateTaxonomyRequest,
    responses((status = 200, body = LexiconTaxonomy), (status = 400, description = "字段或路径无效"),
        (status = 401, description = "未登录"), (status = 403, description = "需要目录编辑权限"),
        (status = 404, description = "配置不存在或类别不符"), (status = 409, description = "名称或版本冲突"),
        (status = 422, description = "请求结构无效"), (status = 500, description = "数据库错误")))]
pub async fn update(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<TaxonomyItemPath>,
    ApiJson(input): ApiJson<UpdateTaxonomyRequest>,
) -> Result<Json<LexiconTaxonomy>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("lexicon_settings.edit_taxonomies")?;
    let zh = text(input.name_zh, "name_zh")?;
    let en = text(input.name_en, "name_en")?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "lexicon_settings.edit_taxonomies").await?;
    lock_item(&mut tx, &path, input.base_revision).await?;
    let item = sqlx::query_as::<_, LexiconTaxonomy>("UPDATE catalog.lexicon_taxonomies SET name_zh=$3,name_en=$4,sort_order=$5,revision=revision+1,updated_at=now(),updated_by_admin_id=$6 WHERE kind=$1 AND id=$2 RETURNING id,code,name_zh,name_en,sort_order,revision,created_at,updated_at")
        .bind(path.kind.as_str()).bind(path.id).bind(zh).bind(en).bind(input.sort_order).bind(auth.subject)
        .fetch_one(&mut *tx).await.map_err(database_error)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(item))
}

#[utoipa::path(delete, path = "/api/v1/admin/lexicon/taxonomies/{kind}/{id}", tag = "admin-catalog",
    security(("bearer_auth" = [])), params(TaxonomyItemPath, DeleteRevisionQuery),
    responses((status = 204, description = "删除成功"), (status = 400, description = "路径或版本无效"),
        (status = 401, description = "未登录"), (status = 403, description = "需要词库配置删除权限"),
        (status = 404, description = "配置不存在或类别不符"), (status = 409, description = "版本冲突"),
        (status = 500, description = "数据库错误")))]
pub async fn remove(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<TaxonomyItemPath>,
    ApiQuery(query): ApiQuery<DeleteRevisionQuery>,
) -> Result<StatusCode, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("lexicon_settings.delete")?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "lexicon_settings.delete").await?;
    lock_item(&mut tx, &path, query.base_revision).await?;
    sqlx::query("DELETE FROM catalog.lexicon_taxonomies WHERE kind=$1 AND id=$2")
        .bind(path.kind.as_str())
        .bind(path.id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}
