use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::speech::Voice;

use super::dto::{VoiceCapabilities, VoiceListResponse, VoiceResponse};

#[derive(Debug, Clone)]
pub struct VoiceRecord {
    pub id: Uuid,
    pub alias: String,
    pub provider_version: String,
    pub voice: Voice,
    pub min_rate_percent: i16,
    pub max_rate_percent: i16,
    pub min_pitch_semitones: i16,
    pub max_pitch_semitones: i16,
    pub gender: String,
}

#[derive(Debug, Clone)]
pub struct CacheRecord {
    pub object_key: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct PreviewRepository {
    pool: PgPool,
}

#[async_trait]
pub trait PreviewRepositoryPort: Send + Sync {
    async fn list_voices(&self) -> Result<VoiceListResponse, sqlx::Error>;
    async fn voice_by_alias(&self, alias: &str) -> Result<Option<VoiceRecord>, sqlx::Error>;
    async fn active_cache(&self, hash: &[u8]) -> Result<Option<CacheRecord>, sqlx::Error>;
    async fn cache_by_hash(&self, hash: &[u8]) -> Result<Option<CacheRecord>, sqlx::Error>;
    async fn save_cache(
        &self,
        request_hash: &[u8],
        content_hash: &[u8],
        voice_id: Uuid,
        object_key: &str,
        size_bytes: i64,
    ) -> Result<Option<String>, sqlx::Error>;
}

impl PreviewRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_voices(&self) -> Result<VoiceListResponse, sqlx::Error> {
        let rows = sqlx::query(
            r#"SELECT alias, locale, gender, styles, min_rate_percent, max_rate_percent,
                      min_pitch_semitones, max_pitch_semitones
               FROM speech.voices WHERE enabled ORDER BY alias"#,
        )
        .fetch_all(&self.pool)
        .await?;
        let items = rows
            .into_iter()
            .map(|row| {
                let mut voice_styles = styles(&row.get::<Value, _>("styles")).unwrap_or_default();
                voice_styles.sort();
                VoiceResponse {
                    alias: row.get("alias"),
                    locale: row.get("locale"),
                    gender: row.get("gender"),
                    capabilities: VoiceCapabilities {
                        styles: voice_styles,
                        min_rate_percent: row.get("min_rate_percent"),
                        max_rate_percent: row.get("max_rate_percent"),
                        min_pitch_semitones: row.get("min_pitch_semitones"),
                        max_pitch_semitones: row.get("max_pitch_semitones"),
                    },
                }
            })
            .collect();
        Ok(VoiceListResponse { items })
    }

    pub async fn voice_by_alias(&self, alias: &str) -> Result<Option<VoiceRecord>, sqlx::Error> {
        let Some(row) = sqlx::query(
            r#"SELECT id, alias, provider, provider_voice_id, locale, gender, styles,
                      min_rate_percent, max_rate_percent, min_pitch_semitones,
                      max_pitch_semitones, provider_version
               FROM speech.voices WHERE alias = $1 AND enabled"#,
        )
        .bind(alias)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let style_values = styles(&row.get::<Value, _>("styles")).ok_or(sqlx::Error::Decode(
            "speech voice styles must be an array of strings".into(),
        ))?;
        let voice = Voice::new(
            row.get::<String, _>("provider"),
            row.get::<String, _>("provider_voice_id"),
            row.get::<String, _>("locale"),
            style_values,
        )
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        Ok(Some(VoiceRecord {
            id: row.get("id"),
            alias: row.get("alias"),
            provider_version: row.get("provider_version"),
            voice,
            min_rate_percent: row.get("min_rate_percent"),
            max_rate_percent: row.get("max_rate_percent"),
            min_pitch_semitones: row.get("min_pitch_semitones"),
            max_pitch_semitones: row.get("max_pitch_semitones"),
            gender: row.get("gender"),
        }))
    }

    pub async fn active_cache(&self, hash: &[u8]) -> Result<Option<CacheRecord>, sqlx::Error> {
        sqlx::query_as::<_, (String, DateTime<Utc>)>(
            "SELECT object_key, expires_at FROM speech.preview_cache WHERE request_hash = $1 AND expires_at > now()",
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(|(object_key, expires_at)| CacheRecord { object_key, expires_at }))
    }

    pub async fn cache_by_hash(&self, hash: &[u8]) -> Result<Option<CacheRecord>, sqlx::Error> {
        sqlx::query_as::<_, (String, DateTime<Utc>)>(
            "SELECT object_key, expires_at FROM speech.preview_cache WHERE request_hash = $1",
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|(object_key, expires_at)| CacheRecord {
                object_key,
                expires_at,
            })
        })
    }

    pub async fn save_cache(
        &self,
        request_hash: &[u8],
        content_hash: &[u8],
        voice_id: Uuid,
        object_key: &str,
        size_bytes: i64,
    ) -> Result<Option<String>, sqlx::Error> {
        let expires_at = Utc::now() + Duration::hours(24);
        let previous = sqlx::query_scalar::<_, String>(
            r#"INSERT INTO speech.preview_cache
               (request_hash, voice_id, content_hash, object_key, mime_type, size_bytes, expires_at)
               VALUES ($1, $2, $3, $4, 'audio/mpeg', $5, $6)
               ON CONFLICT (request_hash) DO UPDATE SET
                 voice_id = EXCLUDED.voice_id, content_hash = EXCLUDED.content_hash,
                 object_key = EXCLUDED.object_key, mime_type = EXCLUDED.mime_type,
                 size_bytes = EXCLUDED.size_bytes, created_at = now(), expires_at = EXCLUDED.expires_at
               WHERE speech.preview_cache.expires_at <= now()
               RETURNING object_key"#,
        )
        .bind(request_hash)
        .bind(voice_id)
        .bind(content_hash)
        .bind(object_key)
        .bind(size_bytes)
        .bind(expires_at)
        .fetch_optional(&self.pool)
        .await?;
        Ok(previous)
    }
}

#[async_trait]
impl PreviewRepositoryPort for PreviewRepository {
    async fn list_voices(&self) -> Result<VoiceListResponse, sqlx::Error> {
        PreviewRepository::list_voices(self).await
    }

    async fn voice_by_alias(&self, alias: &str) -> Result<Option<VoiceRecord>, sqlx::Error> {
        PreviewRepository::voice_by_alias(self, alias).await
    }

    async fn active_cache(&self, hash: &[u8]) -> Result<Option<CacheRecord>, sqlx::Error> {
        PreviewRepository::active_cache(self, hash).await
    }

    async fn cache_by_hash(&self, hash: &[u8]) -> Result<Option<CacheRecord>, sqlx::Error> {
        PreviewRepository::cache_by_hash(self, hash).await
    }

    async fn save_cache(
        &self,
        request_hash: &[u8],
        content_hash: &[u8],
        voice_id: Uuid,
        object_key: &str,
        size_bytes: i64,
    ) -> Result<Option<String>, sqlx::Error> {
        PreviewRepository::save_cache(
            self,
            request_hash,
            content_hash,
            voice_id,
            object_key,
            size_bytes,
        )
        .await
    }
}

fn styles(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}
