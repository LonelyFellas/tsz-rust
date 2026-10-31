//! Answer-only playback; selection facts are separate from grading/rewards.
use super::repository::*;
use crate::{
    api::{ApiPath, ApiQuery},
    auth::extract::AuthUser,
    dictionary::{
        dto::DictionaryAudio,
        service::{own_audio, playable_audio},
    },
    error::{AppError, ErrorCode},
    lexicon::{dto::AdminWordV3, published::PublishedContentV3},
    platform::storage::{ObjectKey, StorageSpace},
    speech::{locale::AccentGroup, playback},
    state::AppState,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LearningAudioAccent {
    Uk,
    Us,
}
impl LearningAudioAccent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Uk => "uk",
            Self::Us => "us",
        }
    }
    fn group(self) -> AccentGroup {
        match self {
            Self::Uk => AccentGroup::Uk,
            Self::Us => AccentGroup::Us,
        }
    }
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct LearningAudioQuery {
    pub accent: LearningAudioAccent,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningAudioSelection {
    pub publication_id: Uuid,
    pub target_id: Uuid,
    pub asset_id: Uuid,
    pub locale: String,
    pub accent_group: LearningAudioAccent,
    pub source_dialect: crate::lexicon::dto::Dialect,
    #[schema(minimum = 1)]
    pub effective_rank: i32,
    #[schema(minimum = 1)]
    pub candidate_count: i32,
    pub url: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningQuestionAudio {
    pub policy_version: String,
    #[schema(minimum = 1)]
    pub occurrence: i64,
    /// Null means the fixed publication has no usable formal audio. Never synthesize.
    #[schema(required = true)]
    pub audio: Option<LearningAudioSelection>,
}

#[utoipa::path(get,path="/api/v1/me/learning-runs/{id}/questions/{question_id}/audio",tag="learning_tasks",security(("bearer_auth"=[])),
    params(("id"=Uuid,Path),("question_id"=Uuid,Path),LearningAudioQuery),
    responses((status=200,body=LearningQuestionAudio),(status=400),(status=401),(status=403,description="尚未作答"),(status=404,description="非本人轮次或来源不可用"),(status=409,description="学习设置或账号状态不可用"),(status=503)))]
pub async fn read(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath((id, question_id)): ApiPath<(Uuid, Uuid)>,
    ApiQuery(query): ApiQuery<LearningAudioQuery>,
) -> Result<(HeaderMap, Json<LearningQuestionAudio>), AppError> {
    let value = read_in(&state, &auth, id, question_id, query.accent).await?;
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    Ok((headers, Json(value)))
}

async fn read_in(
    state: &AppState,
    auth: &AuthUser,
    id: Uuid,
    question_id: Uuid,
    accent: LearningAudioAccent,
) -> Result<LearningQuestionAudio, AppError> {
    let task_id = run_task(&state.pool, auth, id).await?;
    let lists = task_lists(&state.pool, auth, task_id).await?;
    let mut tx = begin(&state.pool, auth, &lists).await?;
    task(&mut tx, auth, task_id).await?;
    // Same lock order as answers; serializes concurrent first reads and new retries.
    let run = run(&mut tx, auth, id).await?;
    let qs = questions(&mut tx, id).await?;
    let q = qs
        .iter()
        .find(|q| q.id == question_id)
        .ok_or_else(unavailable)?;
    if !answers(&mut tx, id).await?.contains_key(&question_id) {
        return Err(AppError::forbidden(ErrorCode::Forbidden, "请先完成作答"));
    }
    let access = sources_for_questions(&mut tx, auth, &lists, &run, &qs).await?;
    if !access.available(q) {
        return Err(unavailable());
    }
    let (publication, form_ids): (Option<Uuid>, Vec<Uuid>) =
        sqlx::query_as("SELECT publication_id,form_ids FROM learning_questions WHERE id=$1")
            .bind(question_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let mut response = LearningQuestionAudio {
        policy_version: playback::POLICY.into(),
        occurrence: 1,
        audio: None,
    };
    if let (Some(entry), Some(publication)) = (q.entry_id, publication) {
        let snapshot: serde_json::Value = sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id=$1 AND entry_id=$2 AND content_schema_version=3")
            .bind(publication).bind(entry).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.ok_or_else(unavailable)?;
        let word: AdminWordV3 = serde_json::from_value(snapshot).map_err(AppError::internal)?;
        let native = PublishedContentV3 {
            forms: word.forms,
            meanings: word.meanings,
        };
        // The entry is the stable vocabulary target; a sense/form change never resets its occurrence.
        if let Some(form) = native
            .forms
            .pos
            .iter()
            .flat_map(|p| &p.forms)
            .find(|f| form_ids.contains(&f.id))
        {
            response.occurrence = sqlx::query_scalar("SELECT count(*) FROM learning_questions WHERE run_id=$1 AND entry_id=$2 AND position<=$3")
                .bind(id).bind(entry).bind(q.position).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
            let (variant, pronunciations) = match &form.regional_variants {
                crate::lexicon::dto::WordRegionalVariantsV3::Common { common } => {
                    (common.id, &common.pronunciations)
                }
                crate::lexicon::dto::WordRegionalVariantsV3::UkUs { uk, us } => match accent {
                    LearningAudioAccent::Uk => (uk.id, &uk.pronunciations),
                    LearningAudioAccent::Us => (us.id, &us.pronunciations),
                },
            };
            let targets: Vec<_> = std::iter::once(variant)
                .chain(pronunciations.iter().map(|p| p.id))
                .collect();
            let all = playable_audio(&mut tx, own_audio(&native)).await?;
            let saved = sqlx::query("SELECT target_id,asset_id,occurrence,effective_rank,candidate_count FROM learning_question_audio WHERE question_id=$1 AND accent=$2")
                .bind(question_id).bind(accent.as_str()).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
            let pool: Vec<DictionaryAudio> = targets
                .iter()
                .filter(|target| {
                    saved
                        .as_ref()
                        .is_none_or(|row| row.get::<Uuid, _>("target_id") == **target)
                })
                .find_map(|target| {
                    let candidates: Vec<_> = all
                        .iter()
                        .filter(|a| {
                            a.target_id == *target
                                && a.accent_group == accent.group()
                                && matches!(
                                    (a.source_dialect, accent),
                                    (crate::lexicon::dto::Dialect::Common, _)
                                        | (
                                            crate::lexicon::dto::Dialect::Uk,
                                            LearningAudioAccent::Uk
                                        )
                                        | (
                                            crate::lexicon::dto::Dialect::Us,
                                            LearningAudioAccent::Us
                                        )
                                )
                        })
                        .cloned()
                        .collect();
                    (!candidates.is_empty()).then_some(candidates)
                })
                .unwrap_or_default();
            let selected = if let Some(saved) = saved {
                response.occurrence = saved.get("occurrence");
                pool.iter()
                    .find(|a| {
                        a.target_id == saved.get::<Uuid, _>("target_id")
                            && a.asset_id == saved.get::<Uuid, _>("asset_id")
                    })
                    .map(|a| {
                        (
                            a.clone(),
                            saved.get::<i32, _>("effective_rank"),
                            saved.get::<i32, _>("candidate_count"),
                        )
                    })
            } else if let Some(index) =
                playback::select(pool.len(), response.occurrence, rand::random::<f64>())
            {
                let chosen = &pool[index];
                sqlx::query("INSERT INTO learning_question_audio(question_id,accent,publication_id,target_id,asset_id,policy_version,occurrence,effective_rank,candidate_count) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(question_id,accent) DO NOTHING")
                    .bind(question_id).bind(accent.as_str()).bind(publication).bind(chosen.target_id).bind(chosen.asset_id).bind(playback::POLICY).bind(response.occurrence).bind(index as i32+1).bind(pool.len() as i32).execute(&mut *tx).await.map_err(AppError::internal)?;
                Some((chosen.clone(), index as i32 + 1, pool.len() as i32))
            } else {
                None
            };
            if let Some((chosen, rank, count)) = selected {
                let key: String = sqlx::query_scalar("SELECT object_key FROM lexicon.audio_assets WHERE id=$1 AND reclamation_started_at IS NULL")
                    .bind(chosen.asset_id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
                // Persist before signing: an object-store failure must not resample.
                learner(&mut tx, auth).await?;
                tx.commit().await.map_err(AppError::internal)?;
                let storage = state
                    .object_storage
                    .get(&StorageSpace::parse("audio").expect("constant space"))
                    .map_err(|_| {
                        AppError::unavailable(
                            ErrorCode::AudioStorageNotConfigured,
                            "正式音频存储暂不可用",
                        )
                    })?;
                let key = ObjectKey::parse(key).map_err(AppError::internal)?;
                storage.stat(&key).await.map_err(|_| unavailable())?;
                let signed = storage.presign_read(&key).await.map_err(|_| {
                    AppError::unavailable(ErrorCode::SpeechStorageUnavailable, "音频服务暂不可用")
                })?;
                response.audio = Some(LearningAudioSelection {
                    publication_id: publication,
                    target_id: chosen.target_id,
                    asset_id: chosen.asset_id,
                    locale: chosen.locale,
                    accent_group: accent,
                    source_dialect: chosen.source_dialect,
                    effective_rank: rank,
                    candidate_count: count,
                    url: signed.url().into(),
                    expires_at: chrono::DateTime::from(
                        std::time::SystemTime::now() + signed.expires_in(),
                    ),
                });
                return Ok(response);
            }
        }
    }
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(response)
}
fn unavailable() -> AppError {
    AppError::not_found("该题正式音频暂不可用")
}
