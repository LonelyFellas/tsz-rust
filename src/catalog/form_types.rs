//! 全局词形类型目录；编码稳定，展示字段可编辑，引用保护由事务和外键共同保证。
use super::model::{Actor, DeleteRevisionQuery, PartListFilter, PartPath};
use crate::{
    admin::{AdminAuth, authorization::require_super_admin},
    api::{ApiJson, ApiPath, ApiQuery, PaginatedResponse},
    error::{AppError, ErrorCode, ProblemMeta},
    state::AppState,
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, patch},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction, types::Json as SqlJson};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FormTypeCatalogItem {
    pub id: Uuid,
    /// 所属基本词性；原形对所有词性通用，此处为 null。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_of_speech_id: Option<Uuid>,
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub short_name_zh: String,
    pub abbreviation: String,
    pub full_name_en: String,
    pub sort_order: i32,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FormTypeConfig {
    pub id: Uuid,
    /// 所属基本词性；原形对所有词性通用，此处为 null。
    pub part_of_speech_id: Option<Uuid>,
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub short_name_zh: String,
    pub abbreviation: String,
    pub full_name_en: String,
    pub sort_order: i32,
    pub usage_count: i64,
    pub revision: i64,
    pub created_by: Actor,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Actor>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FormTypeListQuery {
    /// code、中文名、英文名、缩写、简洁显示或英文全称的忽略大小写字面子串。
    pub q: Option<String>,
    /// 只看该基本词性名下的词形变化；缺省返回全部（含对所有词性通用的原形）。
    pub part_of_speech_id: Option<Uuid>,
    #[param(default = 1, minimum = 1)]
    pub page: Option<u32>,
    #[param(default = 10, minimum = 1, maximum = 100)]
    pub page_size: Option<u32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFormTypeRequest {
    /// 所属基本词性，必填：词形变化都挂在某个基本词性下。
    pub part_of_speech_id: Uuid,
    #[schema(min_length = 1, max_length = 32, pattern = "^[a-z][a-z0-9_]{0,31}$")]
    pub code: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_zh: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_en: String,
    #[schema(min_length = 1, max_length = 16)]
    pub abbreviation: String,
    #[schema(min_length = 1, max_length = 16)]
    pub short_name_zh: String,
    #[schema(min_length = 1, max_length = 64)]
    pub full_name_en: String,
    pub sort_order: i32,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateFormTypeRequest {
    pub base_revision: i64,
    /// 改挂到另一个基本词性；原形不接受该字段。
    pub part_of_speech_id: Option<Uuid>,
    #[schema(min_length = 1, max_length = 64)]
    pub name_zh: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_en: String,
    #[schema(min_length = 1, max_length = 16)]
    pub abbreviation: String,
    #[schema(min_length = 1, max_length = 16)]
    pub short_name_zh: String,
    #[schema(min_length = 1, max_length = 64)]
    pub full_name_en: String,
    pub sort_order: i32,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", patch(update).delete(remove))
}

macro_rules! select_config { () => { r#"
SELECT to_jsonb(f) || jsonb_build_object(
    'created_by', jsonb_build_object('id', coalesce(creator.id::text, 'system'), 'display_name', coalesce(creator.display_name, '系统')),
    'updated_by', CASE WHEN updater.id IS NULL THEN NULL ELSE jsonb_build_object('id',updater.id::text,'display_name',updater.display_name) END,
    'usage_count', (SELECT count(*) FROM (
        SELECT entry_id FROM lexicon.form_slots WHERE form_type = f.code
        UNION SELECT entry_id FROM lexicon.v3_concrete_forms WHERE form_type = f.code
        UNION SELECT entry_id FROM lexicon.entry_publication_form_type_refs WHERE form_type = f.code
        UNION SELECT entry_id FROM lexicon.sentence_associations WHERE resolved_form_type = f.code
        UNION SELECT entry_id FROM lexicon.v3_phrase_sense_component_usages WHERE target_form_type = f.code
        UNION SELECT entry_id FROM lexicon.v3_phrase_variant_component_usages WHERE target_form_type = f.code
    ) refs))
FROM catalog.form_types f
LEFT JOIN admins creator ON creator.id = f.created_by_admin_id
LEFT JOIN admins updater ON updater.id = f.updated_by_admin_id
"# }; }

macro_rules! scope { () => { r#"
 WHERE ($1::text IS NULL OR strpos(lower(concat_ws(' ',f.code,f.name_zh,f.name_en,f.short_name_zh,f.abbreviation,f.full_name_en)),lower($1)) > 0)
   AND ($2::uuid IS NULL OR f.part_of_speech_id = $2 OR f.part_of_speech_id IS NULL)
"# }; }

fn database_error(error: sqlx::Error) -> AppError {
    if let Some(db) = error.as_database_error() {
        if db.code().as_deref() == Some("23505") {
            let constraint = db.constraint().unwrap_or("");
            let field = [
                "short_name_zh",
                "full_name_en",
                "name_zh",
                "name_en",
                "abbreviation",
                "code",
            ]
            .into_iter()
            .find(|field| constraint.contains(field));
            return AppError::conflict(
                ErrorCode::FormTypeConflict,
                field,
                "form type already exists",
            );
        }
        if matches!(db.code().as_deref(), Some("23503" | "23001")) {
            return AppError::conflict(ErrorCode::FormTypeInUse, None, "form type is in use");
        }
    }
    AppError::internal(error)
}

async fn config(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<FormTypeConfig, AppError> {
    sqlx::query_scalar::<_, SqlJson<FormTypeConfig>>(concat!(select_config!(), " WHERE f.id = $1"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .map(|v| v.0)
        .ok_or_else(|| {
            AppError::not_found_with_code(ErrorCode::FormTypeNotFound, "form type not found")
        })
}

fn text(value: String, field: &'static str, max: usize) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max {
        return Err(AppError::validation(
            ErrorCode::InvalidFormType,
            field,
            "invalid text length",
        ));
    }
    Ok(value.to_owned())
}

/// 词形变化必须挂在已存在的基本词性下；不存在时给出和词性接口一致的 404。
async fn require_part(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<(), AppError> {
    let exists =
        sqlx::query_scalar::<_, bool>("SELECT true FROM catalog.parts_of_speech WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?
            .is_some();
    if exists {
        Ok(())
    } else {
        Err(AppError::not_found_with_code(
            ErrorCode::PartOfSpeechNotFound,
            "part of speech not found",
        ))
    }
}

async fn bump(tx: &mut Transaction<'_, Postgres>) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE",
    )
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

#[utoipa::path(get, path="/api/v1/admin/settings/form-types", tag="admin-catalog", security(("bearer_auth"=[])), params(FormTypeListQuery), responses((status=200, body=PaginatedResponse<FormTypeConfig>), (status=400, description="查询参数非法"), (status=403, description="需要超级管理员")))]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<FormTypeListQuery>,
) -> Result<Json<PaginatedResponse<FormTypeConfig>>, AppError> {
    require_super_admin(&state, &auth).await?;
    let filter = PartListFilter {
        q: query
            .q
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
        page: query.page.unwrap_or(1),
        page_size: query.page_size.unwrap_or(10),
    };
    let pos_filter = query.part_of_speech_id;
    if filter.page == 0 || !(1..=100).contains(&filter.page_size) {
        return Err(AppError::bad_request(
            ErrorCode::InvalidQuery,
            "invalid pagination",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(database_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;

    let total = sqlx::query_scalar::<_, i64>(concat!(
        "SELECT count(*) FROM catalog.form_types f",
        scope!()
    ))
    .bind(&filter.q)
    .bind(pos_filter)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    let items = sqlx::query_scalar::<_, SqlJson<FormTypeConfig>>(concat!(
        select_config!(),
        scope!(),
        " ORDER BY f.sort_order,f.created_at,f.id LIMIT $3 OFFSET $4"
    ))
    .bind(&filter.q)
    .bind(pos_filter)
    .bind(filter.limit())
    .bind(filter.offset())
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?
    .into_iter()
    .map(|v| v.0)
    .collect();
    tx.commit().await.map_err(database_error)?;
    Ok(Json(PaginatedResponse {
        items,
        pagination: filter.pagination(total),
    }))
}

#[utoipa::path(post, path="/api/v1/admin/settings/form-types", tag="admin-catalog", security(("bearer_auth"=[])), request_body=CreateFormTypeRequest, responses((status=201, body=FormTypeConfig), (status=400, description="字段非法"), (status=403, description="需要超级管理员"), (status=404, description="所属基本词性不存在"), (status=409, description="配置冲突")))]
pub async fn create(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(request): ApiJson<CreateFormTypeRequest>,
) -> Result<(StatusCode, Json<FormTypeConfig>), AppError> {
    let admin = require_super_admin(&state, &auth).await?;
    if !crate::lexicon::form_types::valid_code(&request.code) {
        return Err(AppError::validation(
            ErrorCode::InvalidFormType,
            "code",
            "invalid form type code",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(database_error)?;
    require_part(&mut tx, request.part_of_speech_id).await?;
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO catalog.form_types(id,part_of_speech_id,code,name_zh,name_en,short_name_zh,abbreviation,full_name_en,sort_order,created_by_admin_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
        .bind(id).bind(request.part_of_speech_id).bind(request.code).bind(text(request.name_zh,"name_zh",64)?).bind(text(request.name_en,"name_en",64)?)
        .bind(text(request.short_name_zh,"short_name_zh",16)?).bind(text(request.abbreviation,"abbreviation",16)?)
        .bind(text(request.full_name_en,"full_name_en",64)?).bind(request.sort_order).bind(admin.id)
        .execute(&mut *tx).await.map_err(database_error)?;
    bump(&mut tx).await?;
    let value = config(&mut tx, id).await?;
    tx.commit().await.map_err(database_error)?;
    Ok((StatusCode::CREATED, Json(value)))
}

async fn lock(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    revision: i64,
) -> Result<FormTypeConfig, AppError> {
    if revision < 1 {
        return Err(AppError::validation(
            ErrorCode::InvalidQuery,
            "base_revision",
            "revision must be positive",
        ));
    }
    sqlx::query("SELECT id FROM catalog.form_types WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
    let value = config(tx, id).await?;
    if value.revision != revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("base_revision"),
            "configuration changed",
        )
        .with_meta(ProblemMeta {
            current_revision: Some(value.revision),
            ..ProblemMeta::default()
        }));
    }
    Ok(value)
}

#[utoipa::path(patch, path="/api/v1/admin/settings/form-types/{id}", tag="admin-catalog", security(("bearer_auth"=[])), params(PartPath), request_body=UpdateFormTypeRequest, responses((status=200, body=FormTypeConfig), (status=400, description="字段非法或试图给原形指定所属词性"), (status=403, description="需要超级管理员"), (status=404, description="配置或所属基本词性不存在"), (status=409, description="配置或版本冲突")))]
pub async fn update(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<PartPath>,
    ApiJson(request): ApiJson<UpdateFormTypeRequest>,
) -> Result<Json<FormTypeConfig>, AppError> {
    let admin = require_super_admin(&state, &auth).await?;
    let mut tx = state.pool.begin().await.map_err(database_error)?;
    let current = lock(&mut tx, path.id, request.base_revision).await?;
    // 原形对所有词性通用，不接受归属；其余词形保持原归属或改挂到另一个词性。
    let part_of_speech_id = if current.code == "base" {
        if request.part_of_speech_id.is_some() {
            return Err(AppError::validation(
                ErrorCode::InvalidFormType,
                "part_of_speech_id",
                "base form type is global",
            ));
        }
        None
    } else {
        let target = request
            .part_of_speech_id
            .or(current.part_of_speech_id)
            .ok_or_else(|| {
                AppError::validation(
                    ErrorCode::InvalidFormType,
                    "part_of_speech_id",
                    "form type must belong to a part of speech",
                )
            })?;
        require_part(&mut tx, target).await?;
        Some(target)
    };
    sqlx::query("UPDATE catalog.form_types SET part_of_speech_id=$9,name_zh=$2,name_en=$3,short_name_zh=$4,abbreviation=$5,full_name_en=$6,sort_order=$7,updated_by_admin_id=$8,updated_at=now(),revision=revision+1 WHERE id=$1")
        .bind(path.id).bind(text(request.name_zh,"name_zh",64)?).bind(text(request.name_en,"name_en",64)?)
        .bind(text(request.short_name_zh,"short_name_zh",16)?).bind(text(request.abbreviation,"abbreviation",16)?)
        .bind(text(request.full_name_en,"full_name_en",64)?).bind(request.sort_order).bind(admin.id).bind(part_of_speech_id)
        .execute(&mut *tx).await.map_err(database_error)?;
    bump(&mut tx).await?;
    let value = config(&mut tx, path.id).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(Json(value))
}

#[utoipa::path(delete, path="/api/v1/admin/settings/form-types/{id}", tag="admin-catalog", security(("bearer_auth"=[])), params(PartPath,DeleteRevisionQuery), responses((status=204, description="删除成功"), (status=400, description="版本非法"), (status=403, description="需要超级管理员"), (status=404, description="配置不存在"), (status=409, description="版本冲突、原形或已引用")))]
pub async fn remove(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<PartPath>,
    ApiQuery(query): ApiQuery<DeleteRevisionQuery>,
) -> Result<StatusCode, AppError> {
    require_super_admin(&state, &auth).await?;
    let mut tx = state.pool.begin().await.map_err(database_error)?;
    let value = lock(&mut tx, path.id, query.base_revision).await?;
    if value.code == "base" {
        return Err(AppError::conflict(
            ErrorCode::FormTypeRequired,
            None,
            "base form type cannot be deleted",
        ));
    }
    if value.usage_count > 0 {
        return Err(
            AppError::conflict(ErrorCode::FormTypeInUse, None, "form type is in use").with_meta(
                ProblemMeta {
                    usage_count: Some(value.usage_count),
                    ..ProblemMeta::default()
                },
            ),
        );
    }
    sqlx::query("DELETE FROM catalog.form_types WHERE id=$1")
        .bind(path.id)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    bump(&mut tx).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(StatusCode::NO_CONTENT)
}
