use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use tracing::Instrument;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    SpeechOptions, SpeechProvider, SynthesisRequest,
    generated_source::{GeneratedSpeechSource, invalid, voice_input},
    preview::PreviewRepository,
};
use crate::{
    admin::{AdminAuth, authorization::require_active_admin},
    api::ApiJson,
    error::{AppError, ErrorCode},
    lexicon::{
        audio_assets::dto::{AudioAsset, AudioAssetGender, AudioAssetLocale},
        dto::VoiceSettingV3,
    },
    platform::storage::{ObjectContentType, ObjectKey, ObjectStore, PutOptions, StorageSpace},
    state::AppState,
};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerateVoiceAudioRequest {
    pub request_id: Uuid,
    pub source: GeneratedSpeechSource,
    pub setting: VoiceSettingV3,
    #[serde(default)]
    pub profile: Option<crate::lexicon::dto::VoiceProfileV3>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GenerateVoiceAudioResponse {
    pub asset: AudioAsset,
}

fn unavailable() -> AppError {
    AppError::unavailable(
        ErrorCode::SpeechProviderUnavailable,
        "正式音频生成暂不可用，请稍后重试",
    )
}
fn storage_error() -> AppError {
    AppError::unavailable(
        ErrorCode::SpeechStorageUnavailable,
        "音频存储暂不可用，请使用相同请求重试",
    )
}
pub(crate) fn effect_hash(
    request: &SynthesisRequest,
    provider_version: &str,
) -> Result<Vec<u8>, AppError> {
    let ssml = super::build_ssml(request).map_err(|_| invalid())?;
    let value = serde_json::json!([
        request.voice().provider(),
        request.voice().provider_voice_id(),
        provider_version,
        request.options().output_format().azure_value(),
        ssml,
    ]);
    Ok(Sha256::digest(serde_json::to_vec(&value).map_err(AppError::internal)?).to_vec())
}

pub(crate) async fn prepare_effect(
    conn: &mut sqlx::PgConnection,
    source: &GeneratedSpeechSource,
    setting: &VoiceSettingV3,
) -> Result<Vec<u8>, AppError> {
    prepare_effect_with_forms(conn, source, setting, None, None).await
}

async fn prepare_effect_with_forms(
    conn: &mut sqlx::PgConnection,
    source: &GeneratedSpeechSource,
    setting: &VoiceSettingV3,
    current: Option<(Uuid, &crate::lexicon::dto::DraftFormsStepContentV3)>,
    batch: Option<&crate::lexicon::service::v3_publication::PublicationBatchContext>,
) -> Result<Vec<u8>, AppError> {
    let (request, version) = prepare_request(conn, source, setting, current, batch).await?;
    effect_hash(&request, &version)
}

pub(crate) async fn prepare_request(
    conn: &mut sqlx::PgConnection,
    source: &GeneratedSpeechSource,
    setting: &VoiceSettingV3,
    current: Option<(Uuid, &crate::lexicon::dto::DraftFormsStepContentV3)>,
    batch: Option<&crate::lexicon::service::v3_publication::PublicationBatchContext>,
) -> Result<(SynthesisRequest, String), AppError> {
    let row = sqlx::query("SELECT provider,provider_voice_id,provider_version,locale,styles,min_rate_percent,max_rate_percent,min_pitch_semitones,max_pitch_semitones FROM speech.voices WHERE alias=$1 AND enabled")
        .bind(&setting.voice_id).fetch_optional(&mut *conn).await.map_err(AppError::internal)?.ok_or_else(invalid)?;
    let voice = super::Voice::new(
        row.get::<String, _>("provider"),
        row.get::<String, _>("provider_voice_id"),
        row.get::<String, _>("locale"),
        serde_json::from_value::<Vec<String>>(row.get("styles")).map_err(AppError::internal)?,
    )
    .map_err(|_| invalid())?;
    if !super::capabilities::synthesis_available(voice.provider_voice_id(), voice.locale())
        || setting
            .locale
            .is_none_or(|locale| locale.as_str() != voice.locale())
        || setting.rate_percent < row.get("min_rate_percent")
        || setting.rate_percent > row.get("max_rate_percent")
        || i16::from(setting.pitch_semitones.unwrap_or(0)) < row.get("min_pitch_semitones")
        || i16::from(setting.pitch_semitones.unwrap_or(0)) > row.get("max_pitch_semitones")
    {
        return Err(invalid());
    }
    source.validate_locale(voice.locale())?;
    source.validate_model(&voice)?;
    let content = source
        .resolve_with_forms(conn, voice.locale(), current, batch)
        .await?;
    let options = SpeechOptions::new(
        &voice,
        setting.style.clone(),
        setting.rate_percent,
        setting.pitch_semitones.unwrap_or(0),
        setting.volume_percent.unwrap_or(100),
    )
    .and_then(|options| {
        options.with_expression(
            &voice,
            setting
                .style_degree
                .as_ref()
                .and_then(serde_json::Number::as_f64),
            setting.role.clone(),
        )
    })
    .map_err(|_| invalid())?
    .with_output_format(setting.output_format.unwrap_or_default());
    let request = SynthesisRequest::new(voice, options, content).map_err(|_| invalid())?;
    Ok((request, row.get("provider_version")))
}

fn in_progress() -> AppError {
    AppError::conflict(
        ErrorCode::SpeechPreviewInProgress,
        None,
        "音频正在生成，请稍后重试",
    )
}

// Runtime DB pools have ten connections; leave capacity for authorization and saves
// while a bounded number of generated-audio requests hold their transaction leases.
static GENERATION_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

#[derive(Clone)]
pub struct GeneratedAudioService {
    pool: PgPool,
    provider: Option<Arc<dyn SpeechProvider>>,
    storage: Option<Arc<dyn ObjectStore>>,
}

impl GeneratedAudioService {
    pub fn new(
        pool: PgPool,
        provider: Option<Arc<dyn SpeechProvider>>,
        storage: Option<Arc<dyn ObjectStore>>,
    ) -> Self {
        Self {
            pool,
            provider,
            storage,
        }
    }

    async fn authorize(&self, actor: Uuid, security_version: i64) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        let authorization = crate::admin::permissions::lock(&mut tx, actor).await?;
        let current: i64 = sqlx::query_scalar("SELECT security_version FROM admins WHERE id=$1")
            .bind(actor)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        if current != security_version {
            return Err(AppError::unauthorized(
                ErrorCode::InvalidToken,
                "invalid token",
            ));
        }
        authorization.require("speech.generate")?;
        if ![
            "words.create",
            "words.edit",
            "sentences.create",
            "sentences.edit",
        ]
        .iter()
        .any(|key| authorization.has(key))
        {
            return Err(AppError::forbidden(
                ErrorCode::Forbidden,
                "生成正式音频需要内容编辑权限",
            ));
        }
        tx.commit().await.map_err(AppError::internal)
    }

    pub async fn generate(
        &self,
        actor: Uuid,
        security_version: i64,
        input: GenerateVoiceAudioRequest,
    ) -> Result<GenerateVoiceAudioResponse, AppError> {
        self.authorize(actor, security_version).await?;
        let permit = GENERATION_SLOTS
            .acquire()
            .await
            .map_err(|_| in_progress())?;
        let mut lease = self.pool.begin().await.map_err(AppError::internal)?;
        let acquired: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,1))")
                .bind(format!("generated:{actor}:{}", input.request_id))
                .fetch_one(&mut *lease)
                .await
                .map_err(AppError::internal)?;
        if !acquired {
            return Err(in_progress());
        }
        // One generation per actor keeps the request lease from exhausting the small DB pool.
        let actor_slot: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,2))")
                .bind(format!("generated:{actor}"))
                .fetch_one(&mut *lease)
                .await
                .map_err(AppError::internal)?;
        if !actor_slot {
            return Err(in_progress());
        }
        let service = self.clone();
        tokio::spawn(
            async move {
                let _permit = permit;
                let result = service
                    .generate_locked(actor, security_version, input)
                    .await;
                lease.rollback().await.map_err(AppError::internal)?;
                result
            }
            .instrument(tracing::Span::current()),
        )
        .await
        .map_err(AppError::internal)?
    }

    pub(crate) async fn generate_background(
        &self,
        actor: Uuid,
        security_version: i64,
        input: GenerateVoiceAudioRequest,
    ) -> Result<GenerateVoiceAudioResponse, AppError> {
        let _permit = GENERATION_SLOTS
            .acquire()
            .await
            .map_err(|_| in_progress())?;
        let lock = sqlx::postgres::PgAdvisoryLock::new(format!(
            "dubbing-generation:{actor}:{}",
            input.request_id
        ));
        let connection = self.pool.acquire().await.map_err(AppError::internal)?;
        let sqlx::Either::Left(guard) = lock
            .try_acquire(connection)
            .await
            .map_err(AppError::internal)?
        else {
            return Err(in_progress());
        };
        let result = self.generate_locked(actor, security_version, input).await;
        guard.release_now().await.map_err(AppError::internal)?;
        result
    }

    async fn generate_locked(
        &self,
        actor: Uuid,
        security_version: i64,
        input: GenerateVoiceAudioRequest,
    ) -> Result<GenerateVoiceAudioResponse, AppError> {
        self.authorize(actor, security_version).await?;
        if input.request_id.is_nil()
            || input.setting.version_id.is_none_or(|id| id.is_nil())
            || input.setting.locale.is_none()
            || input.setting.version_name.as_ref().is_none_or(|name| {
                name.trim().is_empty() || name.chars().count() > 40 || name.contains('\0')
            })
        {
            return Err(invalid());
        }
        if let GeneratedSpeechSource::Text { form_links, .. } = &input.source
            && !form_links.is_empty()
        {
            let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
            crate::admin::permissions::lock(&mut tx, actor)
                .await?
                .require("words.access")?;
            tx.commit().await.map_err(AppError::internal)?;
        }
        if let Some(profile) = &input.profile {
            let mut conn = self.pool.acquire().await.map_err(AppError::internal)?;
            super::dubbing::validate_setting(
                &mut conn,
                profile,
                &input.source,
                &input.setting,
                None,
                None,
            )
            .await?;
        }
        let native_locale = input.setting.locale.ok_or_else(invalid)?;
        input.source.validate_locale(native_locale.as_str())?;
        let source = input.source.canonical()?;
        let mut setting = voice_input(&input.setting);
        if !input.setting.voice_id.starts_with("azure-hd-flash-") {
            let hash = Sha256::digest(
                serde_json::to_vec(&serde_json::json!([source, setting]))
                    .map_err(AppError::internal)?,
            )
            .to_vec();
            if let Some(asset) = self.existing(actor, input.request_id, &hash).await? {
                return Ok(GenerateVoiceAudioResponse { asset });
            }
        }
        let voice = PreviewRepository::new(self.pool.clone())
            .voice_by_alias(&input.setting.voice_id)
            .await
            .map_err(AppError::internal)?
            .ok_or_else(|| {
                AppError::not_found_with_code(ErrorCode::SpeechVoiceNotFound, "音色不可用")
            })?;
        if !crate::speech::capabilities::synthesis_available(
            voice.voice.provider_voice_id(),
            voice.voice.locale(),
        ) {
            return Err(AppError::bad_request(
                ErrorCode::InvalidSpeechPreview,
                "音色或模型能力未经当前资源确认，不能生成",
            ));
        }
        input.source.validate_model(&voice.voice)?;
        if voice.voice.is_hd_flash() {
            setting["provider_voice_id"] = voice.voice.provider_voice_id().into();
            setting["provider_version"] = voice.provider_version.clone().into();
            setting["builder_version"] = super::model::HD_FLASH_BUILDER_VERSION.into();
        }
        let identity = serde_json::json!([source, setting]);
        let hash =
            Sha256::digest(serde_json::to_vec(&identity).map_err(AppError::internal)?).to_vec();
        let locale = native_locale.as_str();
        let pitch = input.setting.pitch_semitones.unwrap_or(0);
        if voice.voice.locale() != locale
            || input.setting.rate_percent < voice.min_rate_percent
            || input.setting.rate_percent > voice.max_rate_percent
            || i16::from(pitch) < voice.min_pitch_semitones
            || i16::from(pitch) > voice.max_pitch_semitones
        {
            return Err(invalid());
        }
        let mut conn = self.pool.acquire().await.map_err(AppError::internal)?;
        let content = input.source.resolve(&mut conn, locale).await?;
        drop(conn);
        let resolved_content = serde_json::to_value(&content).map_err(AppError::internal)?;
        let options = SpeechOptions::new(
            &voice.voice,
            input.setting.style.clone(),
            input.setting.rate_percent,
            pitch,
            input.setting.volume_percent.unwrap_or(100),
        )
        .and_then(|options| {
            options.with_expression(
                &voice.voice,
                input
                    .setting
                    .style_degree
                    .as_ref()
                    .and_then(serde_json::Number::as_f64),
                input.setting.role.clone(),
            )
        })
        .map_err(|error| {
            AppError::bad_request(ErrorCode::InvalidSpeechPreview, error.to_string())
        })?;
        let format = input.setting.output_format.unwrap_or_default();
        let options = options.with_output_format(format);
        let request = SynthesisRequest::new(voice.voice, options, content).map_err(|error| {
            AppError::bad_request(ErrorCode::InvalidSpeechPreview, error.to_string())
        })?;
        let effect_hash = effect_hash(&request, &voice.provider_version)?;
        if let Some(asset) = self.existing(actor, input.request_id, &hash).await? {
            return Ok(GenerateVoiceAudioResponse { asset });
        }
        let provider = self.provider.clone().ok_or_else(unavailable)?;
        if provider.provider_name() != request.voice().provider() {
            return Err(unavailable());
        }
        let storage = self.storage.clone().ok_or_else(|| {
            AppError::request_error(ErrorCode::AudioStorageNotConfigured, "音频存储尚未配置")
        })?;
        let gender = AudioAssetGender::parse(&voice.gender.to_lowercase()).ok_or_else(invalid)?;
        let asset_locale = AudioAssetLocale::parse(locale).ok_or_else(invalid)?;
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        let previous: Option<(Uuid,String)> = sqlx::query_as("SELECT a.id,a.object_key FROM lexicon.audio_generations g JOIN lexicon.audio_assets a ON a.id=g.asset_id WHERE g.created_by_admin_id=$1 AND g.request_id=$2 AND a.reclamation_started_at IS NULL FOR UPDATE OF a")
            .bind(actor).bind(input.request_id).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
        let id = previous
            .as_ref()
            .map(|value| value.0)
            .unwrap_or_else(Uuid::now_v7);
        let key = if let Some((_, key)) = &previous {
            ObjectKey::parse(key).map_err(AppError::internal)?
        } else {
            storage
                .qualify_key(
                    ObjectKey::parse(format!("assets/generated/{id}.{}", format.extension()))
                        .map_err(AppError::internal)?,
                )
                .map_err(AppError::internal)?
        };
        let name = format!(
            "voice-{}.{}",
            input.setting.version_id.unwrap(),
            format.extension()
        );
        // The pending asset is not readable until generation becomes ready. Its row makes
        // interrupted object writes discoverable by the normal zero-reference collector.
        if previous.is_none() {
            sqlx::query("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id) VALUES($1,$2,$3,$8,1,$4,$5,$6,$7)")
            .bind(id).bind(key.as_str()).bind(format!("generated/{actor}/{}",input.request_id)).bind(asset_locale.as_str()).bind(gender.as_str()).bind(&name).bind(actor).bind(format.content_type()).execute(&mut *tx).await.map_err(AppError::internal)?;
            sqlx::query("INSERT INTO lexicon.audio_generations(asset_id,request_id,created_by_admin_id,input_hash,source,voice,resolved_content,state) VALUES($1,$2,$3,$4,$5,$6,$7,'pending')")
            .bind(id).bind(input.request_id).bind(actor).bind(&hash).bind(&source).bind(&setting).bind(&resolved_content).execute(&mut *tx).await.map_err(AppError::internal)?;
        } else {
            sqlx::query("UPDATE lexicon.audio_assets SET created_at=now() WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(AppError::internal)?;
            sqlx::query("UPDATE lexicon.audio_generations SET state='pending',resolved_content=$2 WHERE asset_id=$1").bind(id).bind(&resolved_content).execute(&mut *tx).await.map_err(AppError::internal)?;
        }
        sqlx::query("UPDATE lexicon.audio_generations SET effect_hash=$2 WHERE asset_id=$1")
            .bind(id)
            .bind(&effect_hash)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        tx.commit().await.map_err(AppError::internal)?;
        let result = self
            .finish(actor, security_version, id, &key, storage.clone(), request)
            .await;
        if let Err(error) = result {
            // A commit error may have committed successfully. Only compensate after proving
            // the row is still pending; unknown outcomes retain the registered object key.
            let failed = sqlx::query("UPDATE lexicon.audio_generations SET state='failed' WHERE asset_id=$1 AND state='pending'").bind(id).execute(&self.pool).await;
            if failed.is_ok_and(|row| row.rows_affected() == 1) {
                if let Err(error) = storage.delete(&key).await {
                    tracing::warn!(
                        error_kind = crate::safe_log::error_kind(&error),
                        "generated audio compensation failed"
                    );
                }
                return Err(error);
            }
            if self
                .existing(actor, input.request_id, &hash)
                .await?
                .is_none()
            {
                return Err(error);
            }
        }
        self.authorize(actor, security_version).await?;
        self.existing(actor, input.request_id, &hash)
            .await?
            .map(|asset| GenerateVoiceAudioResponse { asset })
            .ok_or_else(unavailable)
    }

    async fn finish(
        &self,
        actor: Uuid,
        security_version: i64,
        id: Uuid,
        key: &ObjectKey,
        storage: Arc<dyn ObjectStore>,
        request: SynthesisRequest,
    ) -> Result<(), AppError> {
        self.authorize(actor, security_version).await?;
        let provider = self.provider.as_ref().ok_or_else(unavailable)?;
        let audio =
            tokio::time::timeout(provider.synthesis_timeout(), provider.synthesize(&request))
                .await
                .map_err(|_| unavailable())?
                .map_err(|_| unavailable())?;
        if audio.bytes.is_empty()
            || audio.content_type != request.options().output_format().content_type()
            || (request.options().output_format() == super::AudioOutputFormat::Wav24Khz16BitMono
                && !request.options().output_format().accepts(&audio.bytes))
            || audio.bytes.len() as u64 > storage.policy().max_object_size()
        {
            return Err(unavailable());
        }
        self.authorize(actor, security_version).await?;
        let size = i64::try_from(audio.bytes.len()).map_err(|_| invalid())?;
        storage
            .put(
                key,
                audio.bytes,
                PutOptions::new(Some(
                    ObjectContentType::parse(request.options().output_format().content_type())
                        .expect("constant MIME"),
                )),
            )
            .await
            .map_err(|_| storage_error())?;
        self.authorize(actor, security_version).await?;
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        sqlx::query("UPDATE lexicon.audio_assets SET size_bytes=$2 WHERE id=$1")
            .bind(id)
            .bind(size)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        sqlx::query("UPDATE lexicon.audio_generations SET state='ready' WHERE asset_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        tx.commit().await.map_err(AppError::internal)
    }

    async fn existing(
        &self,
        actor: Uuid,
        request: Uuid,
        hash: &[u8],
    ) -> Result<Option<AudioAsset>, AppError> {
        let row = sqlx::query("SELECT a.*,g.state,g.input_hash FROM lexicon.audio_generations g JOIN lexicon.audio_assets a ON a.id=g.asset_id WHERE g.created_by_admin_id=$1 AND g.request_id=$2 AND a.reclamation_started_at IS NULL")
            .bind(actor).bind(request).fetch_optional(&self.pool).await.map_err(AppError::internal)?;
        let Some(row) = row else { return Ok(None) };
        if row.get::<Vec<u8>, _>("input_hash") != hash {
            return Err(AppError::conflict(
                ErrorCode::IdempotencyConflict,
                None,
                "请求编号已用于其他配音内容",
            ));
        }
        if row.get::<String, _>("state") != "ready" {
            return Ok(None);
        }
        Ok(Some(AudioAsset {
            id: row.get("id"),
            locale: AudioAssetLocale::parse(&row.get::<String, _>("locale")).ok_or_else(invalid)?,
            gender: AudioAssetGender::parse(&row.get::<String, _>("gender")).ok_or_else(invalid)?,
            content_type: row.get("content_type"),
            size_bytes: row.get("size_bytes"),
            duration_ms: row.get("duration_ms"),
            original_name: row.get("original_name"),
            created_at: row.get("created_at"),
        }))
    }
}

#[utoipa::path(post,path="/api/v1/admin/speech/audio",tag="admin-speech",security(("bearer_auth"=[])),request_body=GenerateVoiceAudioRequest,params(("X-TSZ-Test-Run"=Option<String>,Header,description="独占验收资源批次"),("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型；旧读取隐藏模型配置并保护旧写"),("X-TSZ-Voice-Versions"=Option<String>,Header,description="v2 支持正式音频"),("X-TSZ-Voice-Locales"=Option<String>,Header,description="v1 与版本 v2 组合支持原生英语地区；指定发音必须携带 phoneme_locale")),responses((status=201,body=GenerateVoiceAudioResponse),(status=400,description="内容或配音版本无效"),(status=401,description="未登录"),(status=403,description="没有生成和内容编辑权限"),(status=404,description="音色不可用"),(status=409,description="生成中或请求编号冲突"),(status=501,description="长期音频存储未配置"),(status=503,description="生成或存储失败")))]
pub async fn generate(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiJson(input): ApiJson<GenerateVoiceAudioRequest>,
) -> Result<(StatusCode, HeaderMap, Json<GenerateVoiceAudioResponse>), AppError> {
    require_active_admin(&state, &auth).await?;
    if !crate::lexicon::voice_versions::dubbing_supported(&headers)
        && crate::lexicon::voice_versions::contains_dubbing(
            &serde_json::json!({"profile":input.profile,"source":input.source,"setting":input.setting}),
        )
    {
        return Err(AppError::bad_request(
            ErrorCode::InvalidSpeechPreview,
            crate::lexicon::voice_versions::UPGRADE_MESSAGE,
        ));
    }
    if !crate::lexicon::voice_versions::models_supported(&headers) {
        let model: Option<String> = sqlx::query_scalar(
            "SELECT provider_voice_id FROM speech.voices WHERE alias=$1 AND enabled",
        )
        .bind(&input.setting.voice_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?;
        if model.is_some_and(|id| id.ends_with(":DragonHDFlashLatestNeural")) {
            return Err(AppError::bad_request(
                ErrorCode::InvalidSpeechPreview,
                "该模型需要新版配音能力，请刷新页面",
            ));
        }
    }
    if !crate::lexicon::voice_versions::extended_supported(&headers)
        && input
            .setting
            .locale
            .is_some_and(|locale| !locale.is_legacy())
    {
        return Err(invalid());
    }
    if crate::lexicon::voice_versions::extended_supported(&headers)
        && matches!(
            &input.source,
            GeneratedSpeechSource::Phoneme {
                phoneme_locale: None,
                ..
            }
        )
    {
        return Err(AppError::bad_request(
            ErrorCode::InvalidSpeechPreview,
            "指定发音必须提供源音素地区",
        ));
    }
    if !crate::lexicon::voice_versions::extended_supported(&headers) {
        let gender: Option<String> =
            sqlx::query_scalar("SELECT gender FROM speech.voices WHERE alias=$1 AND enabled")
                .bind(&input.setting.voice_id)
                .fetch_optional(&state.pool)
                .await
                .map_err(AppError::internal)?;
        if gender.as_deref() == Some("neutral") {
            return Err(AppError::bad_request(
                ErrorCode::InvalidSpeechPreview,
                "该音色需要新版配音能力，请刷新页面",
            ));
        }
    }
    let (state, _) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    let service = GeneratedAudioService::new(
        state.pool.clone(),
        state.speech_provider.clone(),
        state
            .object_storage
            .get(&StorageSpace::parse("audio").expect("constant space"))
            .ok(),
    );
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        crate::lexicon::voice_versions::MODELS_HEADER,
        "v1".parse().unwrap(),
    );
    response_headers.insert(
        crate::lexicon::voice_versions::LOCALES_HEADER,
        "v1".parse().unwrap(),
    );
    response_headers.insert(
        axum::http::header::VARY,
        "x-tsz-voice-versions, x-tsz-voice-locales, x-tsz-voice-models"
            .parse()
            .unwrap(),
    );
    Ok((
        StatusCode::CREATED,
        response_headers,
        Json(
            service
                .generate(auth.subject, auth.security_version, input)
                .await?,
        ),
    ))
}

pub(crate) async fn validate_bindings(
    conn: &mut sqlx::PgConnection,
    value: &Value,
) -> Result<(), AppError> {
    validate_bindings_with_forms(conn, value, None, None).await
}

pub(crate) async fn validate_bindings_with_forms(
    conn: &mut sqlx::PgConnection,
    value: &Value,
    current: Option<(Uuid, &crate::lexicon::dto::DraftFormsStepContentV3)>,
    batch: Option<&crate::lexicon::service::v3_publication::PublicationBatchContext>,
) -> Result<(), AppError> {
    for configuration in super::generated_source::configurations(value)? {
        if !configuration
            .setting
            .voice_id
            .starts_with("azure-hd-flash-")
        {
            continue;
        }
        let row =
            sqlx::query("SELECT provider_voice_id,locale,styles FROM speech.voices WHERE alias=$1")
                .bind(&configuration.setting.voice_id)
                .fetch_optional(&mut *conn)
                .await
                .map_err(AppError::internal)?
                .ok_or_else(invalid)?;
        let model = super::Voice::new(
            "azure",
            row.get::<String, _>("provider_voice_id"),
            row.get::<String, _>("locale"),
            serde_json::from_value::<Vec<String>>(row.get("styles")).map_err(AppError::internal)?,
        )
        .map_err(|_| invalid())?;
        configuration.source.validate_model(&model)?;
        let setting = &configuration.setting;
        if setting
            .locale
            .is_none_or(|locale| locale.as_str() != model.locale())
        {
            return Err(invalid());
        }
        let content = configuration
            .source
            .resolve_with_forms(conn, model.locale(), current, batch)
            .await?;
        let options = SpeechOptions::new(
            &model,
            setting.style.clone(),
            setting.rate_percent,
            setting.pitch_semitones.unwrap_or(0),
            setting.volume_percent.unwrap_or(100),
        )
        .and_then(|options| {
            options.with_expression(
                &model,
                setting
                    .style_degree
                    .as_ref()
                    .and_then(serde_json::Number::as_f64),
                setting.role.clone(),
            )
        })
        .map_err(|error| {
            AppError::bad_request(ErrorCode::InvalidSpeechPreview, error.to_string())
        })?;
        SynthesisRequest::new(model, options, content).map_err(|error| {
            AppError::bad_request(ErrorCode::InvalidSpeechPreview, error.to_string())
        })?;
    }
    fn uploaded_ids(value: &Value, ids: &mut Vec<Uuid>) {
        if let Some(items) = value.get("audio_assets").and_then(Value::as_array) {
            ids.extend(items.iter().filter_map(|item| {
                item.get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| Uuid::parse_str(id).ok())
            }));
        }
        match value {
            Value::Object(fields) => fields.values().for_each(|value| uploaded_ids(value, ids)),
            Value::Array(items) => items.iter().for_each(|value| uploaded_ids(value, ids)),
            _ => {}
        }
    }
    let mut uploads = vec![];
    uploaded_ids(value, &mut uploads);
    if !uploads.is_empty()
        && sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM lexicon.audio_generations WHERE asset_id=ANY($1))",
        )
        .bind(&uploads)
        .fetch_one(&mut *conn)
        .await
        .map_err(AppError::internal)?
    {
        return Err(AppError::bad_request(
            ErrorCode::InvalidSpeechPreview,
            "生成音频须绑定配音版本，不能作为上传录音保存",
        ));
    }
    for reference in super::generated_source::references(value)? {
        if let Some(profile) = &reference.profile {
            super::dubbing::validate_setting(
                conn,
                profile,
                &reference.source,
                &reference.setting,
                current,
                batch,
            )
            .await?;
        }
        let row = sqlx::query("SELECT source,voice,resolved_content,state,effect_hash FROM lexicon.audio_generations WHERE asset_id=$1").bind(reference.asset).fetch_optional(&mut *conn).await.map_err(AppError::internal)?.ok_or_else(invalid)?;
        let native_locale = reference.setting.locale.ok_or_else(invalid)?;
        let locale = native_locale.as_str();
        if let Some(effect) = row.get::<Option<Vec<u8>>, _>("effect_hash") {
            if row.get::<String, _>("state") != "ready"
                || effect
                    != prepare_effect_with_forms(
                        conn,
                        &reference.source,
                        &reference.setting,
                        current,
                        batch,
                    )
                    .await?
            {
                return Err(AppError::bad_request(
                    ErrorCode::InvalidSpeechPreview,
                    "正式音频与当前声音效果不一致，请重新生成或移除音频",
                ));
            }
            continue;
        }
        let mut expected_voice = voice_input(&reference.setting);
        if reference.setting.voice_id.starts_with("azure-hd-flash-") {
            let catalog = sqlx::query("SELECT provider_voice_id,provider_version,locale,styles FROM speech.voices WHERE alias=$1")
                .bind(&reference.setting.voice_id).fetch_optional(&mut *conn).await.map_err(AppError::internal)?.ok_or_else(invalid)?;
            let model = super::Voice::new(
                "azure",
                catalog.get::<String, _>("provider_voice_id"),
                catalog.get::<String, _>("locale"),
                serde_json::from_value::<Vec<String>>(catalog.get("styles"))
                    .map_err(AppError::internal)?,
            )
            .map_err(|_| invalid())?;
            reference.source.validate_model(&model)?;
            SpeechOptions::new(
                &model,
                reference.setting.style.clone(),
                reference.setting.rate_percent,
                reference.setting.pitch_semitones.unwrap_or(0),
                reference.setting.volume_percent.unwrap_or(100),
            )
            .map_err(|_| invalid())?;
            expected_voice["provider_voice_id"] =
                catalog.get::<String, _>("provider_voice_id").into();
            expected_voice["provider_version"] =
                catalog.get::<String, _>("provider_version").into();
            expected_voice["builder_version"] = super::model::HD_FLASH_BUILDER_VERSION.into();
        }
        let resolved = serde_json::to_value(
            reference
                .source
                .resolve_with_forms(conn, locale, current, batch)
                .await?,
        )
        .map_err(AppError::internal)?;
        if row.get::<String, _>("state") != "ready"
            || row.get::<Value, _>("source") != reference.source.canonical()?
            || row.get::<Value, _>("voice") != expected_voice
            || row.get::<Value, _>("resolved_content") != resolved
        {
            return Err(AppError::bad_request(
                ErrorCode::InvalidSpeechPreview,
                "正式音频与当前正文、读法或参数不一致，请重新生成或移除音频",
            ));
        }
    }
    Ok(())
}
