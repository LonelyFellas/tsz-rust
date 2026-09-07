use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::dto::{AudioAsset, AudioAssetGender, AudioAssetLocale};

pub struct NewAudioAsset<'a> {
    pub id: Uuid,
    pub object_key: &'a str,
    pub source_key: &'a str,
    pub content_type: &'a str,
    pub size_bytes: i64,
    pub locale: AudioAssetLocale,
    pub gender: AudioAssetGender,
    pub original_name: &'a str,
    pub created_by_admin_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct AudioAssetRecord {
    pub object_key: String,
    pub created_by_admin_id: Uuid,
}

/// source_key 的唯一约束名；并发 confirm 撞它时按「已登记」处理而不是报 500。
pub const SOURCE_KEY_UNIQUE: &str = "lexicon_audio_assets_source_key_unique";

#[derive(Clone)]
pub struct AudioAssetRepository {
    pool: PgPool,
}

impl AudioAssetRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// `duration_ms` 留空：服务端暂不探测时长。
    pub async fn insert(&self, asset: NewAudioAsset<'_>) -> Result<DateTime<Utc>, sqlx::Error> {
        let row = sqlx::query(
            r#"INSERT INTO lexicon.audio_assets
               (id, object_key, source_key, content_type, size_bytes, locale, gender,
                original_name, created_by_admin_id)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               RETURNING created_at"#,
        )
        .bind(asset.id)
        .bind(asset.object_key)
        .bind(asset.source_key)
        .bind(asset.content_type)
        .bind(asset.size_bytes)
        .bind(asset.locale.as_str())
        .bind(asset.gender.as_str())
        .bind(asset.original_name)
        .bind(asset.created_by_admin_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get("created_at"))
    }

    pub async fn find(&self, id: Uuid) -> Result<Option<AudioAssetRecord>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT object_key, created_by_admin_id FROM lexicon.audio_assets WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| AudioAssetRecord {
            object_key: row.get("object_key"),
            created_by_admin_id: row.get("created_by_admin_id"),
        }))
    }

    /// 资产是否已经被某条词条引用（草稿或发布皆算）。
    /// 一旦被引用，任何能读那条词条的管理员都该能试听它——否则多人协作时，
    /// 非上传者在编辑器里看得见录音却点不开。
    pub async fn is_referenced(&self, asset_id: Uuid) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            r#"SELECT EXISTS(
                   SELECT 1 FROM lexicon.v3_audio_asset_references WHERE asset_id = $1
               )"#,
        )
        .bind(asset_id)
        .fetch_one(&self.pool)
        .await
    }

    /// 按暂存键回查已登记的资产，供 confirm 重放与并发冲突走同一条返回路径。
    pub async fn find_by_source_key(
        &self,
        source_key: &str,
    ) -> Result<Option<AudioAsset>, sqlx::Error> {
        let row = sqlx::query(
            r#"SELECT id, locale, gender, content_type, size_bytes, duration_ms,
                      original_name, created_at
               FROM lexicon.audio_assets WHERE source_key = $1"#,
        )
        .bind(source_key)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let decode = |value: String| sqlx::Error::Decode(format!("unexpected {value}").into());
        let locale: String = row.get("locale");
        let gender: String = row.get("gender");
        Ok(Some(AudioAsset {
            id: row.get("id"),
            locale: AudioAssetLocale::parse(&locale).ok_or_else(|| decode(locale))?,
            gender: AudioAssetGender::parse(&gender).ok_or_else(|| decode(gender))?,
            content_type: row.get("content_type"),
            size_bytes: row.get("size_bytes"),
            duration_ms: row.get("duration_ms"),
            original_name: row.get("original_name"),
            created_at: row.get("created_at"),
        }))
    }
}
