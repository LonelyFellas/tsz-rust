use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::{AppError, ErrorCode},
    speech::library::dto::DubbingGroup,
};

mod worker;
pub use worker::{run_worker, sweep};

#[derive(Debug, Serialize, ToSchema)]
pub struct DubbingUpdateSettings {
    pub wait_seconds: i32,
    pub revision: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveDubbingUpdateSettings {
    pub wait_seconds: i32,
    pub base_revision: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DubbingAudioUpdate {
    pub id: Uuid,
    pub group_id: Uuid,
    pub group_name: String,
    pub group_revision: i64,
    pub state: String,
    pub due_at: DateTime<Utc>,
    pub total: i64,
    pub completed: i64,
    pub failed: i64,
    pub skipped: i64,
    pub error_codes: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DubbingAudioUpdates {
    pub updates: Vec<DubbingAudioUpdate>,
}

fn conflict() -> AppError {
    AppError::conflict(
        ErrorCode::RevisionConflict,
        None,
        "任务或设置已变化，请重新载入",
    )
}

pub(crate) async fn enqueue(
    conn: &mut PgConnection,
    actor: Uuid,
    security_version: i64,
    group: Uuid,
    revision: i64,
    groups: (Option<&DubbingGroup>, Option<&DubbingGroup>),
    test_run: Option<&str>,
) -> Result<Uuid, AppError> {
    let (before, after) = groups;
    let global = before.is_some_and(|g| g.is_global) || after.is_some_and(|g| g.is_global);
    let mut areas = if global {
        vec![
            "word".into(),
            "azure".into(),
            "grammar".into(),
            "meaning".into(),
            "example".into(),
        ]
    } else {
        before
            .into_iter()
            .chain(after)
            .flat_map(|g| g.areas.clone())
            .collect::<Vec<String>>()
    };
    let old = sqlx::query("SELECT areas FROM speech.dubbing_audio_updates WHERE group_id=$1 AND state IN ('pending','running') FOR UPDATE")
        .bind(group).fetch_all(&mut *conn).await.map_err(AppError::internal)?;
    for row in old {
        areas.extend(row.get::<Vec<String>, _>("areas"));
    }
    areas.sort();
    areas.dedup();
    sqlx::query("UPDATE speech.dubbing_audio_updates SET state='superseded',updated_at=now() WHERE group_id=$1 AND state IN ('pending','running')")
        .bind(group).execute(&mut *conn).await.map_err(AppError::internal)?;
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO speech.dubbing_audio_updates(id,group_id,group_revision,areas,created_by_admin_id,security_version,group_name,test_run,due_at) SELECT $1,$2,$3,$4,$5,$6,$7,$8,now()+make_interval(secs=>wait_seconds) FROM speech.dubbing_update_settings WHERE singleton")
        .bind(id).bind(group).bind(revision).bind(areas).bind(actor).bind(security_version).bind(after.or(before).map(|group|group.name.as_str()).unwrap_or("已删除分组")).bind(test_run)
        .execute(conn).await.map_err(AppError::internal)?;
    Ok(id)
}

pub(crate) fn ensure_audio_only(
    before: &serde_json::Value,
    after: &serde_json::Value,
) -> Result<(), AppError> {
    fn stripped(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(fields) => serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            if key == "voice_profile" {
                                serde_json::json!({"dubbing":value.get("dubbing")})
                            } else {
                                stripped(value)
                            },
                        )
                    })
                    .collect(),
            ),
            serde_json::Value::Array(items) => items.iter().map(stripped).collect(),
            _ => value.clone(),
        }
    }
    if stripped(before) != stripped(after) {
        return Err(conflict());
    }
    Ok(())
}

async fn settings_on(conn: &mut PgConnection) -> Result<DubbingUpdateSettings, AppError> {
    let row = sqlx::query(
        "SELECT wait_seconds,revision FROM speech.dubbing_update_settings WHERE singleton",
    )
    .fetch_one(conn)
    .await
    .map_err(AppError::internal)?;
    Ok(DubbingUpdateSettings {
        wait_seconds: row.get("wait_seconds"),
        revision: row.get("revision"),
    })
}

#[utoipa::path(get,path="/api/v1/admin/speech/audio-update-settings",tag="admin-speech",security(("bearer_auth"=[])),responses((status=200,body=DubbingUpdateSettings),(status=403)))]
pub async fn settings(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    auth: crate::admin::AdminAuth,
) -> Result<axum::Json<DubbingUpdateSettings>, AppError> {
    crate::admin::permissions::load(&state, &auth)
        .await?
        .require("speech.global_dubbing")?;
    Ok(axum::Json(
        settings_on(&mut *state.pool.acquire().await.map_err(AppError::internal)?).await?,
    ))
}

#[utoipa::path(put,path="/api/v1/admin/speech/audio-update-settings",tag="admin-speech",security(("bearer_auth"=[])),request_body=SaveDubbingUpdateSettings,responses((status=200,body=DubbingUpdateSettings),(status=400),(status=403),(status=409)))]
pub async fn save_settings(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    auth: crate::admin::AdminAuth,
    crate::api::ApiJson(input): crate::api::ApiJson<SaveDubbingUpdateSettings>,
) -> Result<axum::Json<DubbingUpdateSettings>, AppError> {
    if !(0..=3600).contains(&input.wait_seconds) {
        return Err(AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "等待时长必须为 0–3600 秒",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    crate::admin::permissions::lock_session(&mut tx, &auth, "speech.global_dubbing").await?;
    if sqlx::query("UPDATE speech.dubbing_update_settings SET wait_seconds=$1,revision=revision+1 WHERE singleton AND revision=$2")
        .bind(input.wait_seconds).bind(input.base_revision).execute(&mut *tx).await.map_err(AppError::internal)?.rows_affected()!=1 { return Err(conflict()); }
    let result = settings_on(&mut tx).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(axum::Json(result))
}

pub async fn list(pool: &PgPool, group: Uuid) -> Result<DubbingAudioUpdates, AppError> {
    list_in(pool, Some(group)).await
}

async fn list_in(pool: &PgPool, group: Option<Uuid>) -> Result<DubbingAudioUpdates, AppError> {
    let rows=sqlx::query("SELECT j.id,j.group_id,j.group_name,j.group_revision,j.state,j.due_at,count(t.id) AS total,count(t.id) FILTER(WHERE t.state='completed') AS completed,count(t.id) FILTER(WHERE t.state='failed') AS failed,count(t.id) FILTER(WHERE t.state='skipped') AS skipped,COALESCE(array_agg(DISTINCT t.error_code) FILTER(WHERE t.error_code IS NOT NULL),'{}') AS error_codes FROM speech.dubbing_audio_updates j LEFT JOIN speech.dubbing_audio_update_targets t ON t.update_id=j.id WHERE ($1::uuid IS NULL OR j.group_id=$1) GROUP BY j.id ORDER BY j.created_at DESC,j.id DESC LIMIT 20")
        .bind(group).fetch_all(pool).await.map_err(AppError::internal)?;
    Ok(DubbingAudioUpdates {
        updates: rows
            .into_iter()
            .map(|r| DubbingAudioUpdate {
                id: r.get("id"),
                group_id: r.get("group_id"),
                group_name: r.get("group_name"),
                group_revision: r.get("group_revision"),
                state: r.get("state"),
                due_at: r.get("due_at"),
                total: r.get("total"),
                completed: r.get("completed"),
                failed: r.get("failed"),
                skipped: r.get("skipped"),
                error_codes: r.get("error_codes"),
            })
            .collect(),
    })
}

#[utoipa::path(get,path="/api/v1/admin/speech/audio-updates",tag="admin-speech",security(("bearer_auth"=[])),responses((status=200,body=DubbingAudioUpdates),(status=403)))]
pub async fn all_updates(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    auth: crate::admin::AdminAuth,
) -> Result<axum::Json<DubbingAudioUpdates>, AppError> {
    crate::admin::permissions::load(&state, &auth)
        .await?
        .require("speech.global_dubbing")?;
    Ok(axum::Json(list_in(&state.pool, None).await?))
}

#[utoipa::path(get,path="/api/v1/admin/speech/groups/{id}/audio-updates",tag="admin-speech",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,body=DubbingAudioUpdates),(status=403),(status=404)))]
pub async fn get_updates(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    auth: crate::admin::AdminAuth,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::Json<DubbingAudioUpdates>, AppError> {
    crate::admin::permissions::load(&state, &auth)
        .await?
        .require("speech.global_dubbing")?;
    let result = list(&state.pool, id).await?;
    if result.updates.is_empty() {
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM speech.dubbing_groups,jsonb_array_elements(groups) g WHERE g->>'id'=$1)").bind(id.to_string()).fetch_one(&state.pool).await.map_err(AppError::internal)?;
        if !exists {
            return Err(AppError::not_found("分组不存在"));
        }
    }
    Ok(axum::Json(result))
}

pub async fn change(
    pool: &PgPool,
    auth: &crate::admin::AdminAuth,
    id: Uuid,
    retry: bool,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    crate::admin::permissions::lock_session(&mut tx, auth, "speech.global_dubbing").await?;
    // Keep the same lock order as group saves and worker completion.
    sqlx::query("SELECT revision FROM speech.dubbing_groups WHERE singleton FOR UPDATE")
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let row = sqlx::query(
        "SELECT state,group_id FROM speech.dubbing_audio_updates WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("任务不存在"))?;
    let state: String = row.get("state");
    if retry {
        if !["failed", "partial"].contains(&state.as_str()) {
            return Err(conflict());
        }
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM speech.dubbing_audio_updates newer JOIN speech.dubbing_audio_updates current ON current.id=$2 WHERE newer.group_id=$1 AND newer.id<>current.id AND (newer.state IN ('pending','running') OR (newer.created_at,newer.id)>(current.created_at,current.id)))").bind(row.get::<Uuid,_>("group_id")).bind(id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
        if active {
            return Err(conflict());
        }
        sqlx::query("UPDATE speech.dubbing_audio_update_targets SET state='pending',attempts=0,lease_token=NULL,lease_until=NULL,error_code=NULL,retry_at=now() WHERE update_id=$1 AND state='failed'").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("UPDATE speech.dubbing_audio_updates SET state='running',created_by_admin_id=$2,security_version=$3,updated_at=now() WHERE id=$1").bind(id).bind(auth.subject).bind(auth.security_version).execute(&mut *tx).await.map_err(AppError::internal)?;
    } else {
        if state != "pending" {
            return Err(conflict());
        }
        sqlx::query("UPDATE speech.dubbing_audio_updates SET state='cancelled',updated_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    tx.commit().await.map_err(AppError::internal)
}

#[utoipa::path(post,path="/api/v1/admin/speech/audio-updates/{id}/cancel",tag="admin-speech",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=204),(status=403),(status=404),(status=409)))]
pub async fn cancel(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    auth: crate::admin::AdminAuth,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::http::StatusCode, AppError> {
    change(&state.pool, &auth, id, false).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
#[utoipa::path(post,path="/api/v1/admin/speech/audio-updates/{id}/retry",tag="admin-speech",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=204),(status=403),(status=404),(status=409)))]
pub async fn retry(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    auth: crate::admin::AdminAuth,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::http::StatusCode, AppError> {
    change(&state.pool, &auth, id, true).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
