use super::*;
use crate::speech::publication_audio::{
    AudioRequirement, entry_requirements,
    review::{AudioManifest, Owner, Reviewer, confirm_in, confirmed_in, inspect_in, probe_assets},
    sentence_requirements,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckPublicationAudio {
    #[schema(minimum = 1)]
    pub base_revision: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmPublicationAudio {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(min_length = 64, max_length = 64, pattern = "^[0-9a-f]{64}$")]
    pub manifest_hash: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublicationAudioReview {
    pub manifest: AudioManifest,
    pub confirmed: bool,
    #[schema(required = true)]
    pub receipt_id: Option<Uuid>,
}

#[utoipa::path(post, path="/api/v1/admin/lexicon/entries/{id}/publication-audio/check", tag="admin-lexicon",
    security(("bearer_auth"=[])),params(EntryPath),request_body=CheckPublicationAudio,
    responses((status=200,body=PublicationAudioReview),(status=400),(status=401),(status=403),(status=404),(status=409),(status=503)))]
pub async fn check(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<CheckPublicationAudio>,
) -> Result<Json<PublicationAudioReview>, AppError> {
    entry_review(&state, &auth, path.id, input.base_revision, None)
        .await
        .map(Json)
}

#[utoipa::path(post, path="/api/v1/admin/lexicon/entries/{id}/publication-audio/confirm", tag="admin-lexicon",
    security(("bearer_auth"=[])),params(EntryPath),request_body=ConfirmPublicationAudio,
    responses((status=200,body=PublicationAudioReview),(status=400),(status=401),(status=403),(status=404),(status=409),(status=503)))]
pub async fn confirm(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<ConfirmPublicationAudio>,
) -> Result<Json<PublicationAudioReview>, AppError> {
    if input.manifest_hash.len() != 64
        || !input
            .manifest_hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(AppError::validation(
            ErrorCode::InvalidRequestBody,
            "manifest_hash",
            "音频清单摘要无效",
        ));
    }
    entry_review(
        &state,
        &auth,
        path.id,
        input.base_revision,
        Some(&input.manifest_hash),
    )
    .await
    .map(Json)
}

#[utoipa::path(post, path="/api/v1/admin/lexicon/sentences/{id}/publication-audio/check", tag="admin-lexicon",
    security(("bearer_auth"=[])),params(EntryPath),request_body=CheckPublicationAudio,
    responses((status=200,body=PublicationAudioReview),(status=400),(status=401),(status=403),(status=404),(status=409),(status=503)))]
pub async fn check_sentence(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<CheckPublicationAudio>,
) -> Result<Json<PublicationAudioReview>, AppError> {
    sentence_review(&state, &auth, path.id, input.base_revision, None)
        .await
        .map(Json)
}

#[utoipa::path(post, path="/api/v1/admin/lexicon/sentences/{id}/publication-audio/confirm", tag="admin-lexicon",
    security(("bearer_auth"=[])),params(EntryPath),request_body=ConfirmPublicationAudio,
    responses((status=200,body=PublicationAudioReview),(status=400),(status=401),(status=403),(status=404),(status=409),(status=503)))]
pub async fn confirm_sentence(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<ConfirmPublicationAudio>,
) -> Result<Json<PublicationAudioReview>, AppError> {
    if input.manifest_hash.len() != 64
        || !input
            .manifest_hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(AppError::validation(
            ErrorCode::InvalidRequestBody,
            "manifest_hash",
            "音频清单摘要无效",
        ));
    }
    sentence_review(
        &state,
        &auth,
        path.id,
        input.base_revision,
        Some(&input.manifest_hash),
    )
    .await
    .map(Json)
}

async fn sentence_review(
    state: &AppState,
    auth: &AdminAuth,
    id: Uuid,
    revision: i64,
    expected: Option<&str>,
) -> Result<PublicationAudioReview, AppError> {
    require_active_admin(state, auth).await?;
    v3_contract::require_positive_revision("base_revision", revision)?;
    let sentence = {
        let mut connection = state.pool.acquire().await.map_err(AppError::internal)?;
        crate::lexicon::shared_sentences::read_on(&mut connection, id).await?
    };
    if sentence.revision != revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "例句版本已变化，请重新检查音频",
        ));
    }
    review_content(
        state,
        auth,
        Owner::Sentence(id),
        revision,
        sentence_requirements(&sentence.content.sentence.en_text)?,
        None,
        expected,
    )
    .await
}

async fn entry_review(
    state: &AppState,
    auth: &AdminAuth,
    entry: Uuid,
    revision: i64,
    expected: Option<&str>,
) -> Result<PublicationAudioReview, AppError> {
    require_active_admin(state, auth).await?;
    v3_contract::require_positive_revision("base_revision", revision)?;
    if !state.smart_lexicon_v3_flags.read {
        return Err(v3_storage_unavailable());
    }
    let word = service(state).get_v3(entry).await.map_err(map_error)?;
    if word.revision != revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "内容版本已变化，请重新检查音频",
        ));
    }
    review_content(
        state,
        auth,
        Owner::Entry(entry),
        revision,
        entry_requirements(&word)?,
        Some((entry, &word.forms)),
        expected,
    )
    .await
}

async fn review_content(
    state: &AppState,
    auth: &AdminAuth,
    owner: Owner,
    revision: i64,
    requirements: Vec<AudioRequirement>,
    forms: Option<(Uuid, &crate::lexicon::dto::DraftFormsStepContentV3)>,
    expected: Option<&str>,
) -> Result<PublicationAudioReview, AppError> {
    let probes = if requirements.iter().any(|item| item.asset_id.is_some()) {
        let storage = state
            .object_storage
            .get(&crate::platform::storage::StorageSpace::parse("audio").expect("constant space"))
            .map_err(|_| {
                AppError::unavailable(ErrorCode::AudioStorageNotConfigured, "音频存储尚未配置")
            })?;
        probe_assets(&state.pool, owner, &requirements, storage.as_ref()).await?
    } else {
        crate::speech::publication_audio::review::AssetProbes::empty(owner)
    };
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let manifest = inspect_in(
        &mut tx,
        Reviewer {
            actor_id: auth.subject,
            security_version: auth.security_version,
        },
        owner,
        revision,
        requirements,
        forms,
        &probes,
    )
    .await?;
    let receipt = match expected {
        Some(hash) => Some(
            confirm_in(
                &mut tx,
                auth.subject,
                auth.security_version,
                &manifest,
                hash,
            )
            .await?,
        ),
        None => confirmed_in(&mut tx, auth.subject, auth.security_version, &manifest).await?,
    };
    tx.commit().await.map_err(AppError::internal)?;
    Ok(PublicationAudioReview {
        manifest,
        confirmed: receipt.is_some(),
        receipt_id: receipt,
    })
}
