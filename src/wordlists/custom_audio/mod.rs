pub mod dto;
pub mod handler;
pub mod worker;

use std::{collections::HashSet, sync::Arc};

use axum::http::HeaderMap;
use sqlx::PgConnection;
use uuid::Uuid;

use super::service::Tx;
use crate::{
    auth::extract::AuthUser,
    coins::{
        admin_service::coin_error,
        model::{Actor, Amount, Context, Owner, OwnerType},
    },
    error::{AppError, ErrorCode},
    platform::storage::{ObjectStore, StorageSpace},
    state::AppState,
};
use dto::{AudioGeneration, SavedVoice};

pub const PRICE: i64 = 2;

pub struct GenerationContext {
    voices: Vec<SavedVoice>,
    pub test_run: Option<String>,
}

pub async fn prepare(
    tx: &mut Tx<'_>,
    state: &AppState,
    auth: &AuthUser,
    headers: &HeaderMap,
    request: Option<&AudioGeneration>,
) -> Result<Option<GenerationContext>, AppError> {
    let Some(request) = request else {
        return Ok(None);
    };
    if request.item_ids.len() > 10000
        || request.item_ids.iter().collect::<HashSet<_>>().len() != request.item_ids.len()
        || request
            .expected_cost
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0 && n.to_string() == request.expected_cost)
            .is_none()
    {
        return Err(AppError::bad_request(
            ErrorCode::ValidationFailed,
            "语音生成参数无效",
        ));
    }
    let (scoped, test_run) =
        crate::test_resources::scoped_state(state, headers, auth.subject, false)?;
    storage(&scoped, None)?;
    if state.speech_provider.is_none() {
        return Err(AppError::unavailable(
            ErrorCode::SpeechProviderUnavailable,
            "语音生成暂不可用",
        ));
    }
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT provider_voice_id,locale,provider_version FROM speech.voices WHERE enabled AND provider='azure' AND provider_voice_id IN ('en-GB-SoniaNeural','en-US-AriaNeural') ORDER BY locale"
    ).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    if rows.len() != 2 {
        return Err(AppError::unavailable(
            ErrorCode::SpeechVoiceNotFound,
            "默认发音人暂不可用",
        ));
    }
    Ok(Some(GenerationContext {
        voices: rows
            .into_iter()
            .map(|(provider_voice_id, locale, provider_version)| SavedVoice {
                provider_voice_id,
                locale,
                provider_version,
            })
            .collect(),
        test_run,
    }))
}

pub(super) fn storage(
    state: &AppState,
    run: Option<&str>,
) -> Result<Arc<dyn ObjectStore>, AppError> {
    let space = StorageSpace::parse("audio").expect("constant space");
    match run {
        Some(run) => state
            .test_resources
            .as_ref()
            .ok_or_else(|| {
                AppError::unavailable(ErrorCode::SpeechStorageUnavailable, "测试语音存储暂不可用")
            })?
            .registry(Some(run))
            .map_err(AppError::internal)?
            .get(&space)
            .map_err(|_| {
                AppError::unavailable(ErrorCode::SpeechStorageUnavailable, "语音存储暂不可用")
            }),
        None => state.object_storage.get(&space).map_err(|_| {
            AppError::unavailable(ErrorCode::SpeechStorageUnavailable, "语音存储暂不可用")
        }),
    }
}

/// Call after the owner's account and wordlist locks, including legacy content writes.
pub async fn reconcile_in(tx: &mut Tx<'_>, id: Uuid) -> Result<(), AppError> {
    sqlx::query("UPDATE wordlist_audio_jobs j SET is_current=false,state=CASE WHEN state IN ('queued','processing') THEN 'refunding' ELSE state END,lease_until=NULL,updated_at=clock_timestamp() WHERE j.wordlist_id=$1 AND j.is_current AND NOT EXISTS(SELECT 1 FROM wordlist_items i JOIN wordlists w ON w.id=i.wordlist_id WHERE i.wordlist_id=j.wordlist_id AND i.item_id=j.item_id AND btrim(i.custom_word->>'word')=j.word AND w.deleted_at IS NULL)")
        .bind(id).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}

pub async fn generate_in(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    id: Uuid,
    request_key: Uuid,
    request: &AudioGeneration,
    context: &GenerationContext,
) -> Result<(), AppError> {
    let mut new_items = Vec::new();
    for item in &request.item_ids {
        let word: Option<String> = sqlx::query_scalar("SELECT btrim(custom_word->>'word') FROM wordlist_items WHERE wordlist_id=$1 AND item_id=$2 AND custom_word IS NOT NULL")
            .bind(id).bind(item).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
        let word = word.ok_or_else(|| {
            AppError::bad_request(
                ErrorCode::ValidationFailed,
                "只能为本词表的自定义词汇生成语音",
            )
        })?;
        let existing: Option<String> = sqlx::query_scalar("SELECT state FROM wordlist_audio_jobs WHERE wordlist_id=$1 AND item_id=$2 AND is_current FOR UPDATE")
            .bind(id).bind(item).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
        match existing.as_deref() {
            Some("queued" | "processing" | "ready") => continue,
            Some("refunding") => {
                return Err(AppError::conflict(
                    ErrorCode::WordListConflict,
                    None,
                    "语音退款尚未完成，请稍后重试",
                ));
            }
            _ => new_items.push((*item, word)),
        }
    }
    let amount = i64::try_from(new_items.len()).map_err(AppError::internal)? * PRICE;
    if request.expected_cost != amount.to_string() {
        return Err(AppError::conflict(
            ErrorCode::WordListAudioCostChanged,
            None,
            "语音费用已变化，请刷新后确认费用",
        ));
    }
    if amount == 0 {
        return Ok(());
    }
    let payer = Owner {
        owner_type: OwnerType::User,
        owner_id: auth.subject,
    };
    let receipt = crate::coins::service::debit_in(
        tx,
        payer,
        Amount::new(amount).map_err(coin_error)?,
        &Context {
            actor: Actor::Account(payer),
            idempotency_scope: format!("wordlist-audio:{}", auth.subject),
            idempotency_key: request_key.to_string(),
            source_type: "wordlist_custom_audio".into(),
            source_id: format!("{}:{request_key}", auth.subject),
            reason: "自定义词汇英式及美式语音".into(),
            evidence_ref: None,
        },
    )
    .await
    .map_err(coin_error)?;
    let voices = serde_json::to_value(&context.voices).map_err(AppError::internal)?;
    for (item, word) in new_items {
        sqlx::query("UPDATE wordlist_audio_jobs SET is_current=false WHERE wordlist_id=$1 AND item_id=$2 AND is_current")
            .bind(id).bind(item).execute(&mut **tx).await.map_err(AppError::internal)?;
        sqlx::query("INSERT INTO wordlist_audio_jobs(id,wordlist_id,item_id,user_id,word,voices,state,charge_operation_id,test_run) VALUES($1,$2,$3,$4,$5,$6,'queued',$7,$8)")
            .bind(Uuid::now_v7()).bind(id).bind(item).bind(auth.subject).bind(word).bind(&voices).bind(receipt.operation_id).bind(&context.test_run)
            .execute(&mut **tx).await.map_err(AppError::internal)?;
    }
    Ok(())
}

pub async fn ensure_settled(conn: &mut PgConnection, user: Uuid) -> Result<(), AppError> {
    let pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wordlist_audio_jobs WHERE user_id=$1 AND state IN ('queued','processing','refunding'))")
        .bind(user).fetch_one(conn).await.map_err(AppError::internal)?;
    if pending {
        return Err(AppError::conflict(
            ErrorCode::AccountDeletionUnsettledAudio,
            None,
            "请等待语音生成或退款完成后再申请注销",
        ));
    }
    Ok(())
}
