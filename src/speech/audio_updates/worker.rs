use std::{sync::Arc, time::Duration};

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    error::{AppError, ErrorCode},
    lexicon::dto::{VoiceProfileModeV3, VoiceProfileV3, VoiceSettingV3},
    platform::storage::ObjectStore,
    speech::{
        SpeechProvider,
        generated::{GenerateVoiceAudioRequest, GeneratedAudioService},
        generated_source::GeneratedSpeechSource,
    },
};

struct Claim {
    id: Uuid,
    actor: Uuid,
    security_version: i64,
    test_run: Option<String>,
    kind: String,
    owner: Uuid,
    target: Uuid,
    path: String,
    token: Uuid,
}

fn stale() -> AppError {
    AppError::conflict(
        ErrorCode::RevisionConflict,
        None,
        "更新期间正文或推荐已变化",
    )
}

async fn claim(pool: &PgPool) -> Result<Option<Claim>, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    // Group saves take this lock too: discovery sees a complete committed configuration.
    sqlx::query("SELECT revision FROM speech.dubbing_groups WHERE singleton FOR UPDATE")
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let due=sqlx::query("SELECT id,areas,test_run,created_by_admin_id FROM speech.dubbing_audio_updates WHERE state='pending' AND due_at<=now() AND (SELECT enabled FROM speech.dubbing_update_settings WHERE singleton) ORDER BY due_at,id LIMIT 1 FOR UPDATE SKIP LOCKED")
        .fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    if let Some(row) = due {
        let id: Uuid = row.get("id");
        sqlx::query("INSERT INTO speech.dubbing_audio_update_targets(id,update_id,owner_kind,owner_id,target_id,target_path) SELECT gen_random_uuid(),$1,owner_kind,owner_id,target_id,target_path FROM speech.dubbing_dependencies d WHERE area=ANY($2) AND ($3::text IS NULL OR (owner_kind='entry' AND EXISTS(SELECT 1 FROM lexicon.entries e WHERE e.id=d.owner_id AND e.created_by_admin_id=$4)) OR (owner_kind='sentence' AND EXISTS(SELECT 1 FROM lexicon.shared_sentences s WHERE s.id=d.owner_id AND s.created_by_admin_id=$4))) ON CONFLICT DO NOTHING")
            .bind(id).bind(row.get::<Vec<String>,_>("areas")).bind(row.get::<Option<String>,_>("test_run")).bind(row.get::<Uuid,_>("created_by_admin_id")).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("UPDATE speech.dubbing_audio_updates SET state=CASE WHEN EXISTS(SELECT 1 FROM speech.dubbing_audio_update_targets WHERE update_id=$1) THEN 'running' ELSE 'completed' END,updated_at=now() WHERE id=$1")
            .bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    let row=sqlx::query("SELECT t.id,t.update_id,t.owner_kind,t.owner_id,t.target_id,t.target_path,j.created_by_admin_id,j.security_version,j.test_run FROM speech.dubbing_audio_update_targets t JOIN speech.dubbing_audio_updates j ON j.id=t.update_id WHERE j.state='running' AND (SELECT enabled FROM speech.dubbing_update_settings WHERE singleton) AND ((t.state='pending' AND t.retry_at<=now()) OR (t.state='running' AND t.lease_until<=now())) AND t.attempts<3 AND NOT EXISTS(SELECT 1 FROM speech.dubbing_audio_update_targets busy JOIN speech.dubbing_audio_updates active ON active.id=busy.update_id WHERE busy.id<>t.id AND busy.owner_kind=t.owner_kind AND busy.owner_id=t.owner_id AND busy.state='running' AND busy.lease_until>now() AND active.state='running') ORDER BY j.due_at,t.id LIMIT 1 FOR UPDATE OF t SKIP LOCKED")
        .fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    let result = if let Some(row) = row {
        let id = row.get("id");
        let token = Uuid::now_v7();
        sqlx::query("UPDATE speech.dubbing_audio_update_targets SET state='running',attempts=attempts+1,lease_token=$2,lease_until=now()+interval '10 minutes',updated_at=now() WHERE id=$1")
            .bind(id).bind(token).execute(&mut *tx).await.map_err(AppError::internal)?;
        Some(Claim {
            id,
            token,
            actor: row.get("created_by_admin_id"),
            security_version: row.get("security_version"),
            test_run: row.get("test_run"),
            kind: row.get("owner_kind"),
            owner: row.get("owner_id"),
            target: row.get("target_id"),
            path: row.get("target_path"),
        })
    } else {
        None
    };
    // The last crashed attempt must become terminal instead of remaining leased forever.
    sqlx::query("UPDATE speech.dubbing_audio_update_targets SET state='failed',error_code='lease_expired',lease_token=NULL,lease_until=NULL WHERE state='running' AND attempts>=3 AND lease_until<=now()")
        .execute(&mut *tx).await.map_err(AppError::internal)?;
    settle(&mut tx).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}

async fn settle(conn: &mut sqlx::PgConnection) -> Result<(), AppError> {
    sqlx::query("UPDATE speech.dubbing_audio_updates j SET state=CASE WHEN NOT EXISTS(SELECT 1 FROM speech.dubbing_audio_update_targets t WHERE t.update_id=j.id AND t.state='failed') THEN 'completed' WHEN EXISTS(SELECT 1 FROM speech.dubbing_audio_update_targets t WHERE t.update_id=j.id AND t.state='completed') THEN 'partial' ELSE 'failed' END,updated_at=now() WHERE j.state='running' AND NOT EXISTS(SELECT 1 FROM speech.dubbing_audio_update_targets t WHERE t.update_id=j.id AND t.state IN ('pending','running'))")
        .execute(conn).await.map_err(AppError::internal)?;
    Ok(())
}

async fn authorize(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    claim: &Claim,
) -> Result<(), AppError> {
    let permission = crate::admin::permissions::lock(tx, claim.actor).await?;
    permission.require("speech.generate")?;
    let version: i64 = sqlx::query_scalar("SELECT security_version FROM admins WHERE id=$1")
        .bind(claim.actor)
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    if version != claim.security_version {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    let query = if claim.kind == "entry" {
        "SELECT created_by_admin_id FROM lexicon.entries WHERE id=$1 AND archived_at IS NULL AND content_schema_version=3"
    } else {
        "SELECT created_by_admin_id FROM lexicon.shared_sentences WHERE id=$1 AND deleted_at IS NULL"
    };
    let owner: Uuid = sqlx::query_scalar(query)
        .bind(claim.owner)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(stale)?;
    permission.require_owned_action(
        if claim.kind == "entry" {
            "words.edit"
        } else {
            "sentences.edit"
        },
        owner,
    )
}

async fn snapshot(conn: &mut sqlx::PgConnection, claim: &Claim) -> Result<(i64, Value), AppError> {
    let query = if claim.kind == "entry" {
        "SELECT e.revision,jsonb_build_array(p.forms,p.meanings) AS value FROM lexicon.entries e JOIN lexicon.entry_editor_projection p ON p.entry_id=e.id WHERE e.id=$1 AND e.archived_at IS NULL AND e.content_schema_version=3"
    } else {
        "SELECT revision,content->'en_text' AS value FROM lexicon.shared_sentences WHERE id=$1 AND deleted_at IS NULL"
    };
    let row = sqlx::query(query)
        .bind(claim.owner)
        .fetch_optional(conn)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(stale)?;
    Ok((row.get("revision"), row.get("value")))
}

async fn renew(pool: &PgPool, claim: &Claim) -> Result<(), AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    authorize(&mut tx, claim).await?;
    let updated=sqlx::query("UPDATE speech.dubbing_audio_update_targets t SET lease_until=now()+interval '10 minutes' FROM speech.dubbing_audio_updates j WHERE t.id=$1 AND t.lease_token=$2 AND t.state='running' AND t.lease_until>now() AND j.id=t.update_id AND j.state='running' AND (SELECT enabled FROM speech.dubbing_update_settings WHERE singleton)")
        .bind(claim.id).bind(claim.token).execute(&mut *tx).await.map_err(AppError::internal)?;
    if updated.rows_affected() != 1 {
        return Err(stale());
    }
    tx.commit().await.map_err(AppError::internal)
}

async fn effective(
    conn: &mut sqlx::PgConnection,
    profile: &VoiceProfileV3,
    source: &GeneratedSpeechSource,
    path: &str,
    dialect: Option<&str>,
) -> Result<Vec<VoiceSettingV3>, AppError> {
    let side = match source {
        GeneratedSpeechSource::Phoneme {
            phoneme_locale: Some(locale),
            ..
        } => Some(match locale {
            crate::lexicon::dto::PhonemeLocaleV3::EnGb => "gb",
            crate::lexicon::dto::PhonemeLocaleV3::EnUs => "us",
        }),
        _ => {
            if dialect == Some("uk") {
                Some("gb")
            } else if dialect == Some("us") {
                Some("us")
            } else if path.split('/').any(|key| key == "uk") {
                Some("gb")
            } else if path.split('/').any(|key| key == "us") {
                Some("us")
            } else {
                None
            }
        }
    };
    let mut result = vec![];
    for accent in ["gb", "us"]
        .into_iter()
        .filter(|accent| side.is_none_or(|side| side == *accent))
    {
        let locale = if accent == "gb" { "en-GB" } else { "en-US" };
        let content = source.resolve(conn, locale).await?;
        let native = matches!(source, GeneratedSpeechSource::Phoneme { .. }).then_some(locale);
        result.extend(
            super::super::dubbing::resolve_in(conn, profile, accent, &content, native).await?,
        );
    }
    Ok(result)
}

fn request_id(
    claim: &Claim,
    source: &GeneratedSpeechSource,
    setting: &VoiceSettingV3,
    effect_hash: &[u8],
) -> Result<Uuid, AppError> {
    let data = serde_json::json!([
        claim.id,
        source.canonical()?,
        super::super::generated_source::voice_input(setting),
        effect_hash
    ]);
    let digest = Sha256::digest(serde_json::to_vec(&data).map_err(AppError::internal)?);
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    Ok(Uuid::from_bytes(bytes))
}

async fn run(
    pool: &PgPool,
    service: &GeneratedAudioService,
    claim: &Claim,
) -> Result<bool, AppError> {
    renew(pool, claim).await?;
    let mut conn = pool.acquire().await.map_err(AppError::internal)?;
    let (revision, mut value) = snapshot(&mut conn, claim).await?;
    if !super::super::dubbing::dependencies(&value)
        .iter()
        .any(|(target, path, _)| *target == claim.target && path == &claim.path)
    {
        return Ok(false);
    }
    let target = value.pointer(&claim.path).ok_or_else(stale)?;
    let profile: VoiceProfileV3 =
        serde_json::from_value(target["voice_profile"].clone()).map_err(AppError::internal)?;
    if profile.mode() != Some(VoiceProfileModeV3::Inherit) {
        return Ok(false);
    }
    let source = super::super::generated_source::source_at(&value, &claim.path)?;
    let mut settings = effective(
        &mut conn,
        &profile,
        &source,
        &claim.path,
        target.get("dialect").and_then(Value::as_str),
    )
    .await?;
    let mut hashes = vec![];
    for setting in &settings {
        hashes.push(super::super::generated::prepare_effect(&mut conn, &source, setting).await?);
    }
    // Only assets already owned by this target may be reused; cross-content claims remain forbidden.
    let mut old = vec![];
    for setting in &profile.voices {
        if let Some(asset) = setting.audio_asset_id {
            let row=sqlx::query("SELECT g.effect_hash FROM lexicon.audio_generations g JOIN lexicon.audio_assets a ON a.id=g.asset_id WHERE g.asset_id=$1 AND g.state='ready' AND a.reclamation_started_at IS NULL")
                .bind(asset).fetch_optional(&mut *conn).await.map_err(AppError::internal)?;
            if let Some(row) = row {
                let hash = row.get::<Option<Vec<u8>>, _>("effect_hash");
                if let Some(hash) = hash {
                    old.push((hash, asset));
                }
            }
        }
    }
    drop(conn);
    for (setting, hash) in settings.iter_mut().zip(&hashes) {
        if let Some((_, asset)) = old.iter().find(|(old, _)| old == hash) {
            setting.audio_asset_id = Some(*asset);
            continue;
        }
        renew(pool, claim).await?;
        let result = service
            .generate_background(
                claim.actor,
                claim.security_version,
                GenerateVoiceAudioRequest {
                    request_id: request_id(claim, &source, setting, hash)?,
                    source: source.clone(),
                    setting: setting.clone(),
                    profile: Some(profile.clone()),
                },
            )
            .await?;
        setting.audio_asset_id = Some(result.asset.id);
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    authorize(&mut tx, claim).await?;
    sqlx::query("SELECT revision FROM speech.dubbing_groups WHERE singleton FOR UPDATE")
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let valid = sqlx::query("SELECT t.id FROM speech.dubbing_audio_update_targets t JOIN speech.dubbing_audio_updates j ON j.id=t.update_id WHERE t.id=$1 AND t.lease_token=$2 AND t.state='running' AND t.lease_until>clock_timestamp() AND j.state='running' AND (SELECT enabled FROM speech.dubbing_update_settings WHERE singleton) FOR UPDATE OF t,j")
        .bind(claim.id).bind(claim.token).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    if valid.is_none() {
        return Err(stale());
    }
    if claim.kind == "entry" {
        crate::lexicon::repository::LexiconRepository::lock_surface_contexts(
            &mut tx,
            &[claim.owner],
        )
        .await
        .map_err(AppError::internal)?;
    }
    let lock_owner = if claim.kind == "entry" {
        "SELECT id FROM lexicon.entries WHERE id=$1 FOR UPDATE"
    } else {
        "SELECT id FROM lexicon.shared_sentences WHERE id=$1 FOR UPDATE"
    };
    sqlx::query(lock_owner)
        .bind(claim.owner)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(stale)?;
    let (current_revision, current) = snapshot(&mut tx, claim).await?;
    if current_revision != revision || current != value {
        return Err(stale());
    }
    sqlx::query("SELECT pg_advisory_xact_lock(734892105)")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let latest = effective(
        &mut tx,
        &profile,
        &source,
        &claim.path,
        current
            .pointer(&claim.path)
            .and_then(|target| target.get("dialect"))
            .and_then(Value::as_str),
    )
    .await?;
    if latest.len() != settings.len() {
        return Err(stale());
    }
    for ((latest, setting), hash) in latest.iter().zip(&mut settings).zip(&hashes) {
        if latest.version_id != setting.version_id
            || super::super::generated::prepare_effect(&mut tx, &source, latest).await? != *hash
        {
            return Err(stale());
        }
        let registered: Option<Vec<u8>> = sqlx::query_scalar("SELECT g.effect_hash FROM lexicon.audio_generations g JOIN lexicon.audio_assets a ON a.id=g.asset_id WHERE g.asset_id=$1 AND g.state='ready' AND a.reclamation_started_at IS NULL FOR UPDATE OF a")
            .bind(setting.audio_asset_id).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.flatten();
        if registered.as_ref() != Some(hash) {
            return Err(stale());
        }
        setting.version_name.clone_from(&latest.version_name);
    }
    let mut next = profile.clone();
    next.voices = settings;
    next.voices.extend(
        profile
            .voices
            .iter()
            .filter(|old| {
                !old.enabled
                    && !next
                        .voices
                        .iter()
                        .any(|new| new.version_id == old.version_id)
            })
            .cloned()
            .collect::<Vec<_>>(),
    );
    value.pointer_mut(&claim.path).ok_or_else(stale)?["voice_profile"] =
        serde_json::to_value(next).map_err(AppError::internal)?;
    let changed = value != current;
    if changed {
        if claim.kind == "entry" {
            crate::lexicon::service::v3::replace_dubbing_audio(
                &mut tx,
                claim.actor,
                claim.id,
                claim.owner,
                revision,
                &value,
            )
            .await?;
        } else {
            crate::lexicon::shared_sentences::replace_dubbing_audio(
                &mut tx,
                claim.actor,
                claim.owner,
                revision,
                &value,
            )
            .await?;
        }
    }
    let completed=sqlx::query("UPDATE speech.dubbing_audio_update_targets SET state='completed',error_code=NULL,lease_token=NULL,lease_until=NULL,updated_at=now() WHERE id=$1 AND lease_token=$2 AND lease_until>clock_timestamp()")
        .bind(claim.id).bind(claim.token).execute(&mut *tx).await.map_err(AppError::internal)?;
    if completed.rows_affected() != 1 {
        return Err(stale());
    }
    settle(&mut tx).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(true)
}

async fn finish(
    pool: &PgPool,
    claim: &Claim,
    result: Result<bool, AppError>,
) -> Result<(), AppError> {
    let (state, code, retry) = match result {
        Ok(true) => return Ok(()),
        Ok(false) => ("skipped", None, false),
        Err(error) => {
            let retry = matches!(
                error.code(),
                ErrorCode::SpeechProviderUnavailable
                    | ErrorCode::SpeechStorageUnavailable
                    | ErrorCode::SpeechPreviewInProgress
            );
            if error.code() == ErrorCode::RevisionConflict {
                ("skipped", Some("revision_conflict"), false)
            } else {
                ("failed", Some(error.code().descriptor().slug), retry)
            }
        }
    };
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("UPDATE speech.dubbing_audio_update_targets SET state=CASE WHEN $5 AND attempts<3 THEN 'pending' ELSE $3 END,error_code=$4,lease_token=NULL,lease_until=NULL,retry_at=now()+interval '30 seconds',updated_at=now() WHERE id=$1 AND lease_token=$2 AND state='running'")
        .bind(claim.id).bind(claim.token).bind(state).bind(code).bind(retry).execute(&mut *tx).await.map_err(AppError::internal)?;
    settle(&mut tx).await?;
    tx.commit().await.map_err(AppError::internal)
}

pub async fn sweep(
    pool: &PgPool,
    provider: Option<Arc<dyn SpeechProvider>>,
    storage: Option<Arc<dyn ObjectStore>>,
) -> Result<usize, AppError> {
    sweep_in(pool, provider, storage, None).await
}

async fn sweep_in(
    pool: &PgPool,
    provider: Option<Arc<dyn SpeechProvider>>,
    storage: Option<Arc<dyn ObjectStore>>,
    state: Option<&crate::state::AppState>,
) -> Result<usize, AppError> {
    let mut count = 0;
    for _ in 0..8 {
        let Some(claim) = claim(pool).await? else {
            break;
        };
        let selected_storage = match claim.test_run.as_deref() {
            Some(run) => (|| {
                let state = state.ok_or_else(|| {
                    AppError::unavailable(ErrorCode::SpeechStorageUnavailable, "测试批次存储未配置")
                })?;
                let mut headers = axum::http::HeaderMap::new();
                headers.insert(
                    crate::test_resources::HEADER,
                    run.parse().map_err(AppError::internal)?,
                );
                let (scoped, _) =
                    crate::test_resources::scoped_state(state, &headers, claim.actor, true)?;
                scoped
                    .object_storage
                    .get(&"audio".parse().expect("constant space"))
                    .map(Some)
                    .map_err(AppError::internal)
            })(),
            None => Ok(storage.clone()),
        };
        let result = match selected_storage {
            Ok(storage) => {
                let service = GeneratedAudioService::new(pool.clone(), provider.clone(), storage);
                run(pool, &service, &claim).await
            }
            Err(error) => Err(error),
        };
        finish(pool, &claim, result).await?;
        count += 1;
    }
    Ok(count)
}

pub fn run_worker(state: crate::state::AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let storage = state
                .object_storage
                .get(&"audio".parse().expect("constant space"))
                .ok();
            if let Err(error) = sweep_in(
                &state.pool,
                state.speech_provider.clone(),
                storage,
                Some(&state),
            )
            .await
            {
                tracing::warn!(
                    error_code = error.code().descriptor().slug,
                    "dubbing update worker failed"
                );
            }
        }
    });
}
