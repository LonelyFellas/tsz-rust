use std::collections::HashSet;

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use sqlx::types::Json as SqlJson;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    admin::{AdminAuth, permissions},
    api::ApiJson,
    error::{AppError, ErrorCode, ProblemMeta},
    state::AppState,
};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
/// 稳定目录 ID；原形允许搭配任意单词词性，其余词形必须属于所选词性。
pub struct FormSharingMember {
    pub part_of_speech_id: Uuid,
    pub form_type_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormSharingGroup {
    pub id: Uuid,
    #[schema(min_items = 2, max_items = 100)]
    pub members: Vec<FormSharingMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FormSharingTemplate {
    #[schema(minimum = 1)]
    pub revision: i64,
    #[schema(max_items = 100)]
    pub groups: Vec<FormSharingGroup>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
/// 整体替换模板；空 groups 关闭默认共享，不影响既有词条。
pub struct UpdateFormSharingTemplate {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(max_items = 100)]
    pub groups: Vec<FormSharingGroup>,
}

fn invalid() -> AppError {
    AppError::validation(
        ErrorCode::InvalidFormType,
        "groups",
        "invalid or overlapping form sharing groups",
    )
}

#[utoipa::path(get, path = "/api/v1/admin/settings/parts-of-speech/form-sharing", tag = "admin-catalog",
    security(("bearer_auth" = [])),
    responses((status = 200, body = FormSharingTemplate), (status = 401, description = "未登录"),
        (status = 403, description = "需要词库配置查看权限"), (status = 500, description = "数据库错误")))]
pub async fn get(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Json<FormSharingTemplate>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("lexicon_settings.access")?;
    let value = sqlx::query_scalar::<_, SqlJson<FormSharingTemplate>>(
        "SELECT to_jsonb(t) FROM catalog.form_sharing_template t",
    )
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok(Json(value.0))
}

#[utoipa::path(put, path = "/api/v1/admin/settings/parts-of-speech/form-sharing", tag = "admin-catalog",
    security(("bearer_auth" = [])), request_body = UpdateFormSharingTemplate,
    responses((status = 200, body = FormSharingTemplate), (status = 400, description = "组合重复、归属或字段无效"),
        (status = 401, description = "未登录"), (status = 403, description = "需要共享模板编辑权限"),
        (status = 409, description = "版本冲突"), (status = 422, description = "请求结构无效"),
        (status = 500, description = "数据库错误")))]
pub async fn update(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<UpdateFormSharingTemplate>,
) -> Result<Json<FormSharingTemplate>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("lexicon_settings.edit_form_sharing")?;
    if input.base_revision < 1 {
        return Err(AppError::validation(
            ErrorCode::InvalidFormType,
            "base_revision",
            "revision must be positive",
        ));
    }
    if input.groups.len() > 100 {
        return Err(invalid());
    }
    let mut group_ids = HashSet::new();
    let mut labels = HashSet::new();
    for group in &input.groups {
        if !group_ids.insert(group.id) || !(2..=100).contains(&group.members.len()) {
            return Err(invalid());
        }
        for member in &group.members {
            if !labels.insert((member.part_of_speech_id, member.form_type_id)) {
                return Err(invalid());
            }
        }
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "lexicon_settings.edit_form_sharing").await?;
    let revision = sqlx::query_scalar::<_, i64>(
        "SELECT revision FROM catalog.form_sharing_settings WHERE id=TRUE FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    if revision != input.base_revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("base_revision"),
            "template changed; reload before retrying",
        )
        .with_meta(ProblemMeta {
            current_revision: Some(revision),
            ..ProblemMeta::default()
        }));
    }
    // Stable lock order also serializes catalog reparent/delete against validation and insertion.
    let parts = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id,kind FROM catalog.parts_of_speech ORDER BY id FOR SHARE",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let forms = sqlx::query_as::<_, (Uuid, String, Option<Uuid>)>(
        "SELECT id,code,part_of_speech_id FROM catalog.form_types ORDER BY id FOR SHARE",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    for (part_id, form_id) in labels {
        if !parts
            .iter()
            .any(|(id, kind)| *id == part_id && kind == "word")
            || !forms.iter().any(|(id, code, parent)| {
                *id == form_id && (code == "base" || *parent == Some(part_id))
            })
        {
            return Err(invalid());
        }
    }
    sqlx::query("DELETE FROM catalog.form_sharing_groups")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    for (order, group) in input.groups.iter().enumerate() {
        sqlx::query("INSERT INTO catalog.form_sharing_groups(id,sort_order) VALUES($1,$2)")
            .bind(group.id)
            .bind(order as i32)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        for (order, member) in group.members.iter().enumerate() {
            sqlx::query("INSERT INTO catalog.form_sharing_members(group_id,part_of_speech_id,form_type_id,sort_order) VALUES($1,$2,$3,$4)")
                .bind(group.id).bind(member.part_of_speech_id).bind(member.form_type_id).bind(order as i32)
                .execute(&mut *tx).await.map_err(AppError::internal)?;
        }
    }
    sqlx::query("UPDATE catalog.form_sharing_settings SET revision=revision+1,updated_at=now(),updated_by_admin_id=$1 WHERE id=TRUE")
        .bind(auth.subject).execute(&mut *tx).await.map_err(AppError::internal)?;
    sqlx::query("UPDATE catalog.metadata SET version=version+1,updated_at=now() WHERE id=TRUE")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(FormSharingTemplate {
        revision: revision + 1,
        groups: input.groups,
    }))
}
