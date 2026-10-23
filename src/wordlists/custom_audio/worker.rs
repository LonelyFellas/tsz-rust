use std::time::Duration;

use uuid::Uuid;

use super::{dto::SavedVoice, storage};
use crate::{
    error::{AppError, ErrorCode},
    lexicon::dto::RichTextV2,
    platform::storage::{ObjectContentType, ObjectKey, PutOptions},
    speech::{SpeechOptions, SynthesisRequest, Voice},
    state::AppState,
};

type CleanupCandidate = (Uuid, Uuid, Uuid, Uuid, String, Option<String>);

#[derive(sqlx::FromRow)]
struct Job {
    id: Uuid,
    user_id: Uuid,
    wordlist_id: Uuid,
    word: String,
    voices: sqlx::types::Json<Vec<SavedVoice>>,
    state: String,
    attempts: i32,
    test_run: Option<String>,
    lease_available: bool,
    due: bool,
}

struct Attempt {
    job: Job,
    token: Uuid,
    keys: Vec<(SavedVoice, ObjectKey)>,
}

async fn lock_owner_list(tx: &mut super::Tx<'_>, user: Uuid, list: Uuid) -> Result<bool, AppError> {
    let active: Option<bool> =
        sqlx::query_scalar("SELECT status='active' FROM users WHERE id=$1 FOR SHARE")
            .bind(user)
            .fetch_optional(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    sqlx::query("SELECT id FROM wordlists WHERE id=$1 FOR UPDATE")
        .bind(list)
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    Ok(active == Some(true))
}

async fn claim(
    state: &AppState,
    id: Uuid,
    user: Uuid,
    list: Uuid,
) -> Result<Option<Attempt>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let active = lock_owner_list(&mut tx, user, list).await?;
    super::reconcile_in(&mut tx, list).await?;
    let job: Job = sqlx::query_as("SELECT id,user_id,wordlist_id,word,voices,state,attempts,test_run,COALESCE(lease_until<=clock_timestamp(),true) AS lease_available,next_attempt_at<=clock_timestamp() AS due FROM wordlist_audio_jobs WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    if job.state == "ready"
        || job.state == "refunded"
        || !job.due
        || (job.state == "processing" && !job.lease_available)
    {
        tx.commit().await.map_err(AppError::internal)?;
        return Ok(None);
    }
    if job.state == "refunding" || job.attempts >= 3 || !active {
        sqlx::query(
            "UPDATE wordlist_audio_jobs SET state='refunding',lease_until=NULL WHERE id=$1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        crate::coins::service::refund_custom_audio_in(&mut tx, id)
            .await
            .map_err(AppError::internal)?;
        tx.commit().await.map_err(AppError::internal)?;
        return Ok(None);
    }
    let store = match storage(state, job.test_run.as_deref()) {
        Ok(store) => store,
        Err(_) => {
            sqlx::query("UPDATE wordlist_audio_jobs SET attempts=attempts+1,next_attempt_at=clock_timestamp()+interval '30 seconds',state=CASE WHEN attempts>=2 THEN 'refunding' ELSE 'queued' END,lease_until=NULL WHERE id=$1")
                .bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
            tx.commit().await.map_err(AppError::internal)?;
            return Ok(None);
        }
    };
    let token = Uuid::now_v7();
    let mut keys = Vec::new();
    for voice in &job.voices.0 {
        let key = store
            .qualify_key(
                ObjectKey::parse(format!("wordlist-custom/{id}/{token}/{}.mp3", voice.locale))
                    .map_err(AppError::internal)?,
            )
            .map_err(AppError::internal)?;
        sqlx::query("INSERT INTO wordlist_audio_objects(job_id,attempt_token,locale,object_key,cleanup_after) VALUES($1,$2,$3,$4,clock_timestamp()+interval '10 minutes')")
            .bind(id).bind(token).bind(&voice.locale).bind(key.as_str()).execute(&mut *tx).await.map_err(AppError::internal)?;
        keys.push((voice.clone(), key));
    }
    sqlx::query("UPDATE wordlist_audio_jobs SET state='processing',attempts=attempts+1,lease_token=$2,lease_until=clock_timestamp()+interval '5 minutes',updated_at=clock_timestamp() WHERE id=$1")
        .bind(id).bind(token).execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Some(Attempt { job, token, keys }))
}

async fn synthesize(state: &AppState, attempt: &Attempt) -> Result<(), AppError> {
    let provider = state.speech_provider.as_ref().ok_or_else(|| {
        AppError::unavailable(ErrorCode::SpeechProviderUnavailable, "语音服务暂不可用")
    })?;
    let store = storage(state, attempt.job.test_run.as_deref())?;
    for (saved, key) in &attempt.keys {
        let voice = Voice::new(
            "azure",
            &saved.provider_voice_id,
            &saved.locale,
            Vec::<String>::new(),
        )
        .map_err(AppError::internal)?;
        let options = SpeechOptions::new(&voice, None, 0, 0, 100).map_err(AppError::internal)?;
        let request = SynthesisRequest::new(
            voice,
            options,
            RichTextV2 {
                version: 2,
                text: attempt.job.word.clone(),
                annotations: vec![],
            },
        )
        .map_err(AppError::internal)?;
        let audio = provider
            .synthesize(&request)
            .await
            .map_err(AppError::internal)?;
        if audio.bytes.is_empty() || audio.content_type != "audio/mpeg" {
            return Err(AppError::unavailable(
                ErrorCode::SpeechProviderUnavailable,
                "语音生成结果无效",
            ));
        }
        store
            .put(
                key,
                audio.bytes,
                PutOptions::new(Some(
                    ObjectContentType::parse("audio/mpeg").map_err(AppError::internal)?,
                )),
            )
            .await
            .map_err(AppError::internal)?;
        sqlx::query("UPDATE wordlist_audio_objects SET uploaded=true,deleted_at=NULL,cleanup_after=clock_timestamp()+interval '10 minutes' WHERE job_id=$1 AND attempt_token=$2 AND locale=$3")
            .bind(attempt.job.id).bind(attempt.token).bind(&saved.locale).execute(&state.pool).await.map_err(AppError::internal)?;
    }
    Ok(())
}

async fn finish(state: &AppState, attempt: &Attempt, success: bool) -> Result<(), AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let active = lock_owner_list(&mut tx, attempt.job.user_id, attempt.job.wordlist_id).await?;
    super::reconcile_in(&mut tx, attempt.job.wordlist_id).await?;
    let current: Option<(String, bool, i32)> = sqlx::query_as("SELECT state,is_current,attempts FROM wordlist_audio_jobs WHERE id=$1 AND lease_token=$2 FOR UPDATE")
        .bind(attempt.job.id).bind(attempt.token).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    let Some((status, is_current, attempts)) = current else {
        return Ok(());
    };
    if status == "refunded" || status == "ready" {
        return Ok(());
    }
    let lease_valid: bool = sqlx::query_scalar(
        "SELECT COALESCE(lease_until>clock_timestamp(),false) FROM wordlist_audio_jobs WHERE id=$1",
    )
    .bind(attempt.job.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    if status == "processing" && !lease_valid {
        return Ok(());
    }
    if status == "refunding" || !is_current || !active || (!success && attempts >= 3) {
        sqlx::query(
            "UPDATE wordlist_audio_jobs SET state='refunding',lease_until=NULL WHERE id=$1",
        )
        .bind(attempt.job.id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        crate::coins::service::refund_custom_audio_in(&mut tx, attempt.job.id)
            .await
            .map_err(AppError::internal)?;
    } else {
        sqlx::query("UPDATE wordlist_audio_jobs SET state=$2,lease_until=NULL,next_attempt_at=clock_timestamp()+interval '10 seconds',updated_at=clock_timestamp() WHERE id=$1")
            .bind(attempt.job.id).bind(if success { "ready" } else { "queued" }).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    tx.commit().await.map_err(AppError::internal)
}

/// Bounded batches and durable leases keep interrupted requests recoverable.
pub async fn sweep(state: &AppState) -> Result<usize, AppError> {
    let candidates: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as("SELECT id,user_id,wordlist_id FROM wordlist_audio_jobs WHERE state IN ('queued','processing','refunding') AND next_attempt_at<=clock_timestamp() AND (state<>'processing' OR lease_until<=clock_timestamp()) ORDER BY next_attempt_at,id LIMIT 8")
        .fetch_all(&state.pool).await.map_err(AppError::internal)?;
    let mut completed = 0;
    for (id, user, list) in candidates {
        match claim(state, id, user, list).await {
            Ok(Some(attempt)) => {
                let result =
                    tokio::time::timeout(Duration::from_secs(120), synthesize(state, &attempt))
                        .await;
                let success = matches!(result, Ok(Ok(())));
                if !success {
                    tracing::warn!(job_id=%id, "custom audio generation attempt failed");
                }
                if let Err(error) = finish(state, &attempt, success).await {
                    tracing::error!(job_id=%id,error_kind=crate::safe_log::error_kind(&error), "custom audio settlement failed; lease will recover");
                } else {
                    completed += 1;
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(job_id=%id,error_kind=crate::safe_log::error_kind(&error), "custom audio task unavailable; retrying");
                sqlx::query("UPDATE wordlist_audio_jobs SET next_attempt_at=clock_timestamp()+interval '30 seconds' WHERE id=$1 AND state<>'ready'")
                    .bind(id).execute(&state.pool).await.map_err(AppError::internal)?;
            }
        }
    }
    Ok(completed)
}

pub async fn cleanup(state: &AppState) -> Result<(), AppError> {
    let candidates: Vec<CleanupCandidate> = sqlx::query_as("SELECT o.job_id,o.attempt_token,j.user_id,j.wordlist_id,o.object_key,j.test_run FROM wordlist_audio_objects o JOIN wordlist_audio_jobs j ON j.id=o.job_id WHERE o.cleanup_after<=clock_timestamp() AND NOT (o.attempt_token=j.lease_token AND ((j.state='processing' AND j.lease_until>clock_timestamp()) OR (j.state='ready' AND ((j.is_current AND EXISTS(SELECT 1 FROM wordlist_items i JOIN wordlists w ON w.id=i.wordlist_id WHERE i.wordlist_id=j.wordlist_id AND i.item_id=j.item_id AND btrim(i.custom_word->>'word')=j.word AND (w.deleted_at IS NULL OR EXISTS(SELECT 1 FROM wordlist_library_nodes n WHERE n.wordlist_id=w.id AND n.user_id=j.user_id AND n.space='owned')))) OR EXISTS(SELECT 1 FROM wordlist_audio_review_refs r WHERE r.job_id=j.id))))) ORDER BY o.cleanup_after LIMIT 100")
        .fetch_all(&state.pool).await.map_err(AppError::internal)?;
    for (job, token, user, list, key, run) in candidates {
        let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
        lock_owner_list(&mut tx, user, list).await?;
        super::reconcile_in(&mut tx, list).await?;
        let retained: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wordlist_audio_jobs j WHERE j.id=$1 AND j.lease_token=$2 AND ((j.state='processing' AND j.lease_until>clock_timestamp()) OR (j.state='ready' AND (j.is_current OR EXISTS(SELECT 1 FROM wordlist_audio_review_refs r WHERE r.job_id=j.id)))))")
            .bind(job).bind(token).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
        if retained {
            continue;
        }
        let store = storage(state, run.as_deref())?;
        // Recheck tombstones without repeating DELETE: a timed-out PUT can arrive after cleanup.
        let key = ObjectKey::parse(&key).map_err(AppError::internal)?;
        let deletion = match store.stat(&key).await {
            Err(crate::platform::storage::StorageError::ObjectNotFound { .. }) => Ok(()),
            Ok(_) => store.delete(&key).await,
            Err(error) => Err(error),
        };
        match deletion {
            Ok(()) => {
                sqlx::query("UPDATE wordlist_audio_objects SET deleted_at=COALESCE(deleted_at,clock_timestamp()),cleanup_after=clock_timestamp()+interval '1 hour' WHERE object_key=$1")
                    .bind(key.as_str()).execute(&mut *tx).await.map_err(AppError::internal)?;
            }
            Err(error) => {
                tracing::warn!(job_id=%job,error_kind=crate::safe_log::error_kind(&error),"custom audio cleanup will retry");
                sqlx::query("UPDATE wordlist_audio_objects SET cleanup_after=clock_timestamp()+interval '1 minute' WHERE object_key=$1")
                    .bind(key.as_str()).execute(&mut *tx).await.map_err(AppError::internal)?;
            }
        }
        tx.commit().await.map_err(AppError::internal)?;
    }
    Ok(())
}

pub fn run_worker(state: AppState) {
    let cleanup_state = state.clone();
    crate::safe_log::spawn_worker("wordlists/custom_audio_cleanup", async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = cleanup(&cleanup_state).await {
                tracing::error!(
                    error_kind = crate::safe_log::error_kind(&error),
                    "custom audio cleanup failed"
                );
            }
        }
    });
    crate::safe_log::spawn_worker("wordlists/custom_audio", async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = sweep(&state).await {
                tracing::error!(
                    error_kind = crate::safe_log::error_kind(&error),
                    "custom audio worker failed"
                );
            }
        }
    });
}
