use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub struct NewAudioAsset<'a> {
    pub id: Uuid,
    pub object_key: &'a str,
    pub content_type: &'a str,
    pub size_bytes: i64,
    pub locale: &'a str,
    pub gender: &'a str,
    pub original_name: &'a str,
    pub created_by_admin_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct AudioAssetRecord {
    pub object_key: String,
    pub created_by_admin_id: Uuid,
}

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
               (id, object_key, content_type, size_bytes, locale, gender,
                original_name, created_by_admin_id)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
               RETURNING created_at"#,
        )
        .bind(asset.id)
        .bind(asset.object_key)
        .bind(asset.content_type)
        .bind(asset.size_bytes)
        .bind(asset.locale)
        .bind(asset.gender)
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
}
