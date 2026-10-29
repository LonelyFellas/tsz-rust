use futures_util::{StreamExt, stream};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{AudioRequirement, RequirementIssue};
use crate::{
    error::{AppError, ErrorCode},
    lexicon::dto::DraftFormsStepContentV3,
    platform::storage::{ObjectKey, ObjectStore},
};

const POLICY_VERSION: i32 = 1;

#[derive(Debug, Clone, Copy)]
pub struct Reviewer {
    pub actor_id: Uuid,
    pub security_version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = PublicationAudioOwner)]
pub enum Owner {
    Entry(Uuid),
    Sentence(Uuid),
}

impl Owner {
    fn ids(self) -> (Option<Uuid>, Option<Uuid>) {
        match self {
            Self::Entry(id) => (Some(id), None),
            Self::Sentence(id) => (None, Some(id)),
        }
    }
    fn permission(self) -> &'static str {
        match self {
            Self::Entry(_) => "words.publish",
            Self::Sentence(_) => "sentences.publish",
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioManifest {
    owner: Owner,
    #[schema(minimum = 1)]
    revision: i64,
    #[schema(minimum = 1)]
    policy_version: i32,
    items: Vec<AudioRequirement>,
    ready: bool,
    #[schema(min_length = 64, max_length = 64, pattern = "^[0-9a-f]{64}$")]
    manifest_hash: String,
    #[serde(skip)]
    hash: Vec<u8>,
    #[serde(skip)]
    transaction_id: String,
    #[serde(skip)]
    reviewer: Reviewer,
    #[serde(skip)]
    objects_verified: bool,
}

impl AudioManifest {
    pub fn ready(&self) -> bool {
        self.ready
    }
    pub fn manifest_hash(&self) -> &str {
        &self.manifest_hash
    }
    pub fn items(&self) -> &[AudioRequirement] {
        &self.items
    }
}

pub struct AssetProbes {
    owner: Owner,
    records: HashMap<Uuid, AssetProbe>,
}

impl AssetProbes {
    /// Empty probes cannot validate any registered asset during locked inspection.
    pub fn empty(owner: Owner) -> Self {
        Self {
            owner,
            records: HashMap::new(),
        }
    }
}

struct AssetProbe {
    key: String,
    size: i64,
    content_type: String,
    readable: bool,
    checked_at: Instant,
}

/// External IO finishes before the content/voice/asset transaction is locked.
/// The locked inspection subsequently verifies that these exact objects remain
/// registered to the content. Partial/time-limited probes fail closed.
pub async fn probe_assets(
    pool: &sqlx::PgPool,
    owner: Owner,
    items: &[AudioRequirement],
    storage: &dyn ObjectStore,
) -> Result<AssetProbes, AppError> {
    let mut ids = items
        .iter()
        .filter_map(|item| item.asset_id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    let (entry, sentence) = owner.ids();
    let rows: Vec<(Uuid,String,i64,String)> = sqlx::query_as(
        "SELECT a.id,a.object_key,a.size_bytes,a.content_type FROM lexicon.audio_assets a
         WHERE a.id=ANY($1) AND a.reclamation_started_at IS NULL
          AND (EXISTS(SELECT 1 FROM lexicon.v3_audio_asset_references r WHERE r.asset_id=a.id AND r.entry_id=$2)
            OR EXISTS(SELECT 1 FROM lexicon.shared_sentence_audio_asset_references r WHERE r.asset_id=a.id AND r.sentence_id=$3))",
    ).bind(ids).bind(entry).bind(sentence).fetch_all(pool).await.map_err(AppError::internal)?;
    let mut records = HashMap::new();
    let probes = stream::iter(rows)
        .map(|(id, key, size, content_type)| async move {
            let readable = match ObjectKey::parse(&key) {
                Ok(object) => tokio::time::timeout(Duration::from_secs(5), storage.stat(&object))
                    .await
                    .is_ok_and(|result| {
                        result.is_ok_and(|m| {
                            m.content_length == size as u64
                                && m.content_type.as_deref() == Some(content_type.as_str())
                        })
                    }),
                Err(_) => false,
            };
            (
                id,
                AssetProbe {
                    key,
                    size,
                    content_type,
                    readable,
                    checked_at: Instant::now(),
                },
            )
        })
        .buffer_unordered(4);
    tokio::pin!(probes);
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some((id, probe)) = probes.next().await {
            records.insert(id, probe);
        }
    })
    .await;
    Ok(AssetProbes { owner, records })
}

fn conflict(message: &str) -> AppError {
    AppError::conflict(ErrorCode::RevisionConflict, None, message)
}

/// The caller owns the content transaction and must pass requirements derived from
/// that exact server-side content. This function never accepts HTTP JSON directly.
pub async fn inspect_in(
    tx: &mut Transaction<'_, Postgres>,
    reviewer: Reviewer,
    owner: Owner,
    revision: i64,
    items: Vec<AudioRequirement>,
    current_forms: Option<(Uuid, &DraftFormsStepContentV3)>,
    probes: &AssetProbes,
) -> Result<AudioManifest, AppError> {
    inspect_impl(
        tx,
        reviewer,
        owner,
        revision,
        items,
        current_forms,
        InspectionOptions {
            probes: Some(probes),
            batch: None,
        },
    )
    .await
}

struct InspectionOptions<'a> {
    probes: Option<&'a AssetProbes>,
    batch: Option<&'a crate::lexicon::service::v3_publication::PublicationBatchContext>,
}

async fn inspect_impl(
    tx: &mut Transaction<'_, Postgres>,
    reviewer: Reviewer,
    owner: Owner,
    revision: i64,
    mut items: Vec<AudioRequirement>,
    current_forms: Option<(Uuid, &DraftFormsStepContentV3)>,
    options: InspectionOptions<'_>,
) -> Result<AudioManifest, AppError> {
    if options.probes.is_some_and(|probes| probes.owner != owner) {
        return Err(conflict("音频探测与当前内容不匹配"));
    }
    let authorization = crate::admin::permissions::lock(tx, reviewer.actor_id).await?;
    authorization.require(match owner {
        Owner::Entry(_) => "words.access",
        Owner::Sentence(_) => "sentences.access",
    })?;
    let version: i64 = sqlx::query_scalar("SELECT security_version FROM admins WHERE id=$1")
        .bind(reviewer.actor_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    if version != reviewer.security_version {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    let current: Option<i64> = match owner {
        Owner::Entry(id) => sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id=$1 AND archived_at IS NULL FOR UPDATE")
            .bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?,
        Owner::Sentence(id) => sqlx::query_scalar("SELECT revision FROM lexicon.shared_sentences WHERE id=$1 AND deleted_at IS NULL FOR UPDATE")
            .bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?,
    };
    if current != Some(revision) {
        return Err(conflict("内容版本已变化，请重新检查音频"));
    }
    // Use the established owner -> voice catalogue -> asset lock order.
    sqlx::query("SELECT pg_advisory_xact_lock(734892105)")
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    let transaction_id: String = sqlx::query_scalar("SELECT pg_current_xact_id()::text")
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    let (entry, sentence) = owner.ids();
    let mut ids = items.iter().filter_map(|i| i.asset_id).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    // Match only assets already saved under this content's ownership. Upload or
    // generation alone does not establish that an asset belongs to this draft.
    let rows = sqlx::query(
        "SELECT a.id,a.object_key,a.content_type,a.size_bytes,a.locale,g.state,g.effect_hash
         FROM lexicon.audio_assets a LEFT JOIN lexicon.audio_generations g ON g.asset_id=a.id
         WHERE a.id=ANY($1) AND a.reclamation_started_at IS NULL
           AND (EXISTS(SELECT 1 FROM lexicon.v3_audio_asset_references r WHERE r.asset_id=a.id AND r.entry_id=$2)
             OR EXISTS(SELECT 1 FROM lexicon.shared_sentence_audio_asset_references r WHERE r.asset_id=a.id AND r.sentence_id=$3))
         ORDER BY a.id FOR SHARE OF a",
    ).bind(&ids).bind(entry).bind(sentence).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let rows = rows
        .into_iter()
        .map(|row| (row.get::<Uuid, _>("id"), row))
        .collect::<HashMap<_, _>>();
    // Membership alone misses reordered defaults and newly selected versions.
    // Resolve each saved inherited profile once per source and compare the whole
    // ordered selection for every accent represented by that source.
    let mut checked_profiles = HashMap::new();
    for item in &items {
        let (Some(profile), Some(source)) = (&item.profile, &item.source) else {
            continue;
        };
        if profile.mode() != Some(crate::lexicon::dto::VoiceProfileModeV3::Inherit)
            || checked_profiles.contains_key(&item.source_path)
        {
            continue;
        }
        let result = validate_inherited_profile(
            tx,
            profile,
            source,
            &item.source_path,
            item.source_dialect,
            current_forms,
            options.batch,
        )
        .await;
        let valid = match result {
            Ok(valid) => valid,
            Err(error) if error.status_code().is_client_error() => false,
            Err(error) => return Err(error),
        };
        checked_profiles.insert(item.source_path.clone(), valid);
    }
    for item in &mut items {
        if checked_profiles.get(&item.source_path) == Some(&false) {
            item.issue = Some(RequirementIssue::EffectMismatch);
        }
        if item.issue.is_some() {
            continue;
        }
        let Some(row) = item.asset_id.and_then(|id| rows.get(&id)) else {
            item.issue = Some(RequirementIssue::AssetUnavailable);
            continue;
        };
        if row
            .get::<Option<String>, _>("state")
            .is_some_and(|state| state != "ready")
        {
            item.issue = Some(RequirementIssue::AssetUnavailable);
            continue;
        }
        if options.probes.is_some_and(|probes| {
            !probes.records.get(&row.get("id")).is_some_and(|probe| {
                probe.readable
                    && probe.checked_at.elapsed() < Duration::from_secs(30)
                    && probe.key == row.get::<String, _>("object_key")
                    && probe.size == row.get::<i64, _>("size_bytes")
                    && probe.content_type == row.get::<String, _>("content_type")
            })
        }) {
            item.issue = Some(RequirementIssue::ObjectUnavailable);
            continue;
        }
        if let Some(setting) = &item.setting {
            if row.get::<Option<String>, _>("state").as_deref() != Some("ready") {
                item.issue = Some(RequirementIssue::AssetUnavailable);
                continue;
            }
            let Some(source) = &item.source else {
                item.issue = Some(RequirementIssue::InvalidSource);
                continue;
            };
            if let Some(profile) = &item.profile {
                match crate::speech::dubbing::validate_setting(
                    tx,
                    profile,
                    source,
                    setting,
                    current_forms,
                    options.batch,
                )
                .await
                {
                    Ok(()) => {}
                    Err(error) if error.status_code().is_client_error() => {
                        item.issue = Some(RequirementIssue::EffectMismatch);
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            match crate::speech::generated::prepare_request(
                tx,
                source,
                setting,
                current_forms,
                options.batch,
            )
            .await
            {
                Ok((request, version)) => {
                    let expected = crate::speech::generated::effect_hash(&request, &version)?;
                    if row.get::<Option<Vec<u8>>, _>("effect_hash").as_ref() != Some(&expected) {
                        item.issue = Some(RequirementIssue::EffectMismatch);
                    }
                }
                Err(error) if error.status_code().is_client_error() => {
                    item.issue = Some(RequirementIssue::InvalidSource)
                }
                Err(error) => return Err(error),
            }
        } else {
            let locale = row.get::<String, _>("locale");
            if item.default_for.iter().any(|accent| {
                !crate::speech::locale::EnglishLocale::parse(&locale)
                    .is_some_and(|locale| locale.accent_group() == *accent)
            }) {
                item.issue = Some(RequirementIssue::EffectMismatch);
            }
        }
    }
    let ready = items.iter().all(|i| i.issue.is_none());
    let mut binding = serde_json::to_value(&items).map_err(AppError::internal)?;
    for (item, encoded) in items
        .iter()
        .zip(binding.as_array_mut().expect("serialized array"))
    {
        if let Some(source) = &item.source {
            // Pinning a draft link during batch publication changes its metadata,
            // not the reviewed reading. Effective sound is verified separately.
            if let Ok(canonical) = source.canonical() {
                encoded["source"] = canonical;
            }
        }
    }
    let hash = Sha256::digest(
        serde_json::to_vec(&(POLICY_VERSION, owner, revision, binding))
            .map_err(AppError::internal)?,
    )
    .to_vec();
    let manifest_hash = hash.iter().map(|b| format!("{b:02x}")).collect();
    Ok(AudioManifest {
        owner,
        revision,
        policy_version: POLICY_VERSION,
        items,
        ready,
        manifest_hash,
        hash,
        transaction_id,
        reviewer,
        objects_verified: options.probes.is_some(),
    })
}

/// Persist an explicit batch confirmation only after recomputing the manifest in
/// the locked content transaction. A digest supplied by the browser is not proof.
pub async fn confirm_in(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    security_version: i64,
    manifest: &AudioManifest,
    expected_hash: &str,
) -> Result<Uuid, AppError> {
    ensure_transaction(tx, actor, security_version, manifest).await?;
    if !manifest.objects_verified {
        return Err(conflict("确认前必须完成本次音频文件核验"));
    }
    let authorization = crate::admin::permissions::lock(tx, actor).await?;
    authorization.require(manifest.owner.permission())?;
    let version: i64 = sqlx::query_scalar("SELECT security_version FROM admins WHERE id=$1")
        .bind(actor)
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    if version != security_version {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    let (entry, sentence) = manifest.owner.ids();
    let record: Option<(i64, Uuid)> = match manifest.owner {
        Owner::Entry(id) => sqlx::query_as("SELECT revision,created_by_admin_id FROM lexicon.entries WHERE id=$1 AND archived_at IS NULL FOR UPDATE")
            .bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?,
        Owner::Sentence(id) => sqlx::query_as("SELECT revision,created_by_admin_id FROM lexicon.shared_sentences WHERE id=$1 AND deleted_at IS NULL FOR UPDATE")
            .bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?,
    };
    let Some((revision, creator)) = record else {
        return Err(AppError::not_found("内容不存在或已归档"));
    };
    authorization.require_owned_action(manifest.owner.permission(), creator)?;
    if revision != manifest.revision || expected_hash != manifest.manifest_hash {
        return Err(conflict("音频清单已变化，请重新检查并确认"));
    }
    if !manifest.ready {
        return Err(conflict("仍有缺失或不可用音频，不能确认本批"));
    }
    let id = Uuid::now_v7();
    let stored: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO speech.publication_audio_reviews(id,entry_id,sentence_id,revision,actor_id,security_version,policy_version,manifest_hash,manifest)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING RETURNING id",
    ).bind(id).bind(entry).bind(sentence).bind(manifest.revision).bind(actor).bind(security_version)
        .bind(POLICY_VERSION).bind(&manifest.hash).bind(serde_json::to_value(&manifest.items).map_err(AppError::internal)?)
        .fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
    match stored {
        Some(id) => Ok(id),
        None => confirmed_in(tx, actor, security_version, manifest)
            .await?
            .ok_or_else(|| conflict("音频确认状态发生变化，请重试")),
    }
}

pub async fn confirmed_in(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    security_version: i64,
    manifest: &AudioManifest,
) -> Result<Option<Uuid>, AppError> {
    ensure_transaction(tx, actor, security_version, manifest).await?;
    if !manifest.ready {
        return Ok(None);
    }
    let (entry, sentence) = manifest.owner.ids();
    sqlx::query_scalar("SELECT id FROM speech.publication_audio_reviews WHERE actor_id=$1 AND security_version=$2 AND revision=$3 AND policy_version=$4 AND manifest_hash=$5 AND entry_id IS NOT DISTINCT FROM $6 AND sentence_id IS NOT DISTINCT FROM $7")
        .bind(actor).bind(security_version).bind(manifest.revision).bind(POLICY_VERSION).bind(&manifest.hash)
        .bind(entry).bind(sentence).fetch_optional(&mut **tx).await.map_err(AppError::internal)
}

async fn ensure_transaction(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    security_version: i64,
    manifest: &AudioManifest,
) -> Result<(), AppError> {
    let transaction: String = sqlx::query_scalar("SELECT pg_current_xact_id()::text")
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    if transaction != manifest.transaction_id
        || actor != manifest.reviewer.actor_id
        || security_version != manifest.reviewer.security_version
    {
        return Err(conflict("必须由当前发布人在本次事务中重新核验音频清单"));
    }
    Ok(())
}

/// Publication never creates a review receipt. It recomputes the exact registry
/// and effect binding under the caller's locks and consumes an existing receipt.
/// Referenced immutable objects were probed at explicit review time; normal GC
/// cannot delete them while their draft/publication references remain.
pub(crate) async fn require_confirmed_in(
    tx: &mut Transaction<'_, Postgres>,
    reviewer: Reviewer,
    owner: Owner,
    revision: i64,
    items: Vec<AudioRequirement>,
    current_forms: Option<(Uuid, &DraftFormsStepContentV3)>,
    batch: Option<&crate::lexicon::service::v3_publication::PublicationBatchContext>,
) -> Result<Option<Uuid>, AppError> {
    let manifest = inspect_impl(
        tx,
        reviewer,
        owner,
        revision,
        items,
        current_forms,
        InspectionOptions {
            probes: None,
            batch,
        },
    )
    .await?;
    if !manifest.ready {
        return Err(conflict(
            "发布音频缺失、不可用或与当前内容不一致，请先完成音频检查",
        ));
    }
    if manifest.items.is_empty() {
        return Ok(None);
    }
    confirmed_in(tx, reviewer.actor_id, reviewer.security_version, &manifest)
        .await?
        .map(Some)
        .ok_or_else(|| conflict("请先集中审听并确认当前版本的本批音频"))
}

pub(crate) async fn attach_receipt_in(
    tx: &mut Transaction<'_, Postgres>,
    owner: Owner,
    publication: Uuid,
    receipt: Option<Uuid>,
) -> Result<(), AppError> {
    let Some(receipt) = receipt else {
        return Ok(());
    };
    let updated = match owner {
        Owner::Entry(entry) => sqlx::query("UPDATE lexicon.entry_publications p SET audio_review_id=r.id FROM speech.publication_audio_reviews r WHERE p.id=$1 AND p.entry_id=$2 AND r.id=$3 AND r.entry_id=p.entry_id AND r.revision=p.source_revision AND r.actor_id=p.published_by_admin_id AND p.audio_review_id IS NULL")
            .bind(publication).bind(entry).bind(receipt).execute(&mut **tx).await,
        Owner::Sentence(sentence) => sqlx::query("UPDATE lexicon.shared_sentence_publications p SET audio_review_id=r.id FROM speech.publication_audio_reviews r WHERE p.id=$1 AND p.sentence_id=$2 AND r.id=$3 AND r.sentence_id=p.sentence_id AND r.revision=p.source_revision AND r.actor_id=p.published_by_admin_id AND p.audio_review_id IS NULL")
            .bind(publication).bind(sentence).bind(receipt).execute(&mut **tx).await,
    }.map_err(AppError::internal)?;
    if updated.rows_affected() != 1 {
        return Err(conflict("发布版本与音频审听凭据不一致"));
    }
    Ok(())
}

async fn validate_inherited_profile(
    tx: &mut Transaction<'_, Postgres>,
    profile: &crate::lexicon::dto::VoiceProfileV3,
    source: &crate::speech::generated_source::GeneratedSpeechSource,
    path: &str,
    source_dialect: Option<crate::lexicon::dto::Dialect>,
    current: Option<(Uuid, &DraftFormsStepContentV3)>,
    batch: Option<&crate::lexicon::service::v3_publication::PublicationBatchContext>,
) -> Result<bool, AppError> {
    use crate::lexicon::dto::{Dialect, PhonemeLocaleV3};
    use crate::speech::{dubbing::resolve_in, generated_source::voice_input, locale::AccentGroup};
    let side = match source {
        crate::speech::generated_source::GeneratedSpeechSource::Phoneme {
            phoneme_locale: Some(locale),
            ..
        } => Some(if *locale == PhonemeLocaleV3::EnGb {
            Dialect::Uk
        } else {
            Dialect::Us
        }),
        _ => source_dialect
            .filter(|dialect| *dialect != Dialect::Common)
            .or_else(|| {
                path.split('/').find_map(|part| match part {
                    "uk" => Some(Dialect::Uk),
                    "us" => Some(Dialect::Us),
                    _ => None,
                })
            }),
    };
    for accent in [AccentGroup::Uk, AccentGroup::Us] {
        if side.is_some_and(|side| match side {
            Dialect::Uk => accent != AccentGroup::Uk,
            Dialect::Us => accent != AccentGroup::Us,
            Dialect::Common => false,
        }) {
            continue;
        }
        let stored = profile
            .voices
            .iter()
            .filter(|voice| {
                voice.enabled
                    && voice
                        .locale
                        .is_some_and(|locale| locale.accent_group() == accent)
            })
            .collect::<Vec<_>>();
        let locale = if accent == AccentGroup::Uk {
            "en-GB"
        } else {
            "en-US"
        };
        let content = source
            .resolve_with_forms(tx, locale, current, batch)
            .await?;
        let resolved = resolve_in(
            tx,
            profile,
            if accent == AccentGroup::Uk {
                "gb"
            } else {
                "us"
            },
            &content,
            matches!(
                source,
                crate::speech::generated_source::GeneratedSpeechSource::Phoneme { .. }
            )
            .then_some(locale),
        )
        .await?;
        if resolved.len() != stored.len()
            || resolved
                .iter()
                .zip(stored)
                .any(|(latest, saved)| voice_input(latest) != voice_input(saved))
        {
            return Ok(false);
        }
    }
    Ok(true)
}
