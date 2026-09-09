use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::speech::{CatalogVoice, Voice, catalog_snapshot, product_voices, voice_display_name};

use super::{
    CACHE_TTL_HOURS,
    dto::{VoiceCapabilities, VoiceListResponse, VoiceResponse},
};

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
            r#"SELECT alias, provider_voice_id, locale, gender, styles, min_rate_percent, max_rate_percent,
                      min_pitch_semitones, max_pitch_semitones
               FROM speech.voices WHERE enabled AND provider = 'azure' AND locale IN ('en-GB', 'en-US')
               ORDER BY array_position($1, provider_voice_id) NULLS LAST, alias"#,
        )
        .bind(product_voices().iter().map(|v| v.provider_voice_id.as_str()).collect::<Vec<_>>())
        .fetch_all(&self.pool)
        .await?;
        let items = rows
            .into_iter()
            .map(|row| {
                let mut voice_styles = styles(&row.get::<Value, _>("styles")).unwrap_or_default();
                voice_styles.sort();
                VoiceResponse {
                    alias: row.get("alias"),
                    display_name: voice_display_name(
                        &row.get::<String, _>("provider_voice_id"),
                        &row.get::<String, _>("locale"),
                    ),
                    is_common: product_voices()
                        .iter()
                        .any(|v| v.provider_voice_id == row.get::<String, _>("provider_voice_id")),
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

    /// Startup inserts missing catalog voices without overwriting operator-maintained records.
    pub async fn ensure_catalog_voices(&self) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(734892105)")
            .execute(&mut *tx)
            .await?;
        for product in catalog_snapshot() {
            let item = product.catalog_voice();
            sqlx::query(r#"INSERT INTO speech.voices
                (id, alias, provider, provider_voice_id, locale, gender, styles, provider_version)
                SELECT $1,$2,'azure',$3,$4,$5,$6,$7
                WHERE NOT EXISTS (SELECT 1 FROM speech.voices WHERE provider = 'azure' AND provider_voice_id = $3)
                ON CONFLICT (alias) DO NOTHING"#)
                .bind(Uuid::now_v7()).bind(item.alias()).bind(item.voice.provider_voice_id())
                .bind(item.voice.locale()).bind(&item.gender).bind(serde_json::json!(product.styles))
                .bind(item.version()).execute(&mut *tx).await?;
        }
        tx.commit().await
    }

    pub async fn sync_voices(&self, voices: &[CatalogVoice]) -> Result<Vec<String>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        // All instances use the same transaction lock; no schema change or process-local race.
        sqlx::query("SELECT pg_advisory_xact_lock(734892105)")
            .execute(&mut *tx)
            .await?;
        let mut aliases = Vec::with_capacity(voices.len());
        for item in voices {
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT alias FROM speech.voices WHERE provider = $1 AND provider_voice_id = $2 ORDER BY alias LIMIT 1"
            ).bind(item.voice.provider()).bind(item.voice.provider_voice_id())
                .fetch_optional(&mut *tx).await?;
            let styles = serde_json::json!(item.voice.styles().collect::<Vec<_>>());
            let version = item.version();
            let alias = if let Some(alias) = existing {
                sqlx::query(
                    r#"UPDATE speech.voices SET locale = $2, gender = $3, styles = $4,
                           provider_version = $5, updated_at = now()
                       WHERE alias = $1 AND
                       (locale, gender, styles, provider_version) IS DISTINCT FROM ($2, $3, $4, $5)"#
                ).bind(&alias).bind(item.voice.locale()).bind(&item.gender)
                    .bind(&styles).bind(&version).execute(&mut *tx).await?;
                alias
            } else {
                let alias = item.alias();
                sqlx::query(
                    r#"INSERT INTO speech.voices
                       (id, alias, provider, provider_voice_id, locale, gender, styles, provider_version)
                       VALUES ($1,$2,$3,$4,$5,$6,$7,$8)"#
                ).bind(Uuid::now_v7()).bind(&alias).bind(item.voice.provider())
                    .bind(item.voice.provider_voice_id()).bind(item.voice.locale())
                    .bind(&item.gender).bind(&styles).bind(&version)
                    .execute(&mut *tx).await?;
                alias
            };
            aliases.push(alias);
        }
        tx.commit().await?;
        Ok(aliases)
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
        let expires_at = Utc::now() + Duration::hours(CACHE_TTL_HOURS);
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

    /// 删除全部过期 row，返回删除条数。走 `speech_preview_cache_expiry_idx`。
    /// 对应的 OSS 对象不在这里删，由 bucket 生命周期规则回收。
    pub async fn delete_expired(&self) -> Result<u64, sqlx::Error> {
        sqlx::query("DELETE FROM speech.preview_cache WHERE expires_at <= now()")
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
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
