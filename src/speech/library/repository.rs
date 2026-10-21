use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::{
    admin::permissions,
    error::{AppError, ErrorCode},
    speech::{SpeechOptions, Voice},
};

use super::dto::{
    DubbingGroupsResponse, SaveDubbingGroupsRequest, SaveVoiceDescriptionRequest,
    SaveVoiceSettingsRequest, VoiceLibraryItem, VoiceLibraryResponse, VoiceParameters,
};

const ITEMS_SQL: &str = r#"
    SELECT v.alias, d.description_zh, COALESCE(d.revision, 0) AS description_revision,
           s.parameters, COALESCE(s.notes, '') AS notes, COALESCE(s.revision, 0) AS personal_revision
    FROM speech.voices v
    LEFT JOIN speech.voice_descriptions d ON d.voice_id = v.id
    LEFT JOIN speech.admin_voice_settings s ON s.voice_id = v.id AND s.admin_id = $1
    WHERE v.enabled AND v.provider = 'azure' AND v.locale IN ('en-GB', 'en-US')
      AND ($2::text IS NULL OR v.alias = $2)
    ORDER BY v.alias
"#;

pub struct VoiceLibraryRepository {
    pool: PgPool,
}

fn invalid() -> AppError {
    AppError::bad_request(
        ErrorCode::InvalidRequestBody,
        "invalid voice library settings",
    )
}

fn conflict() -> AppError {
    AppError::conflict(
        ErrorCode::RevisionConflict,
        None,
        "voice settings changed; reload before saving",
    )
}

fn item(row: PgRow) -> Result<VoiceLibraryItem, AppError> {
    let parameters: Option<serde_json::Value> =
        row.try_get("parameters").map_err(AppError::internal)?;
    Ok(VoiceLibraryItem {
        voice_alias: row.try_get("alias").map_err(AppError::internal)?,
        description_zh: row.try_get("description_zh").map_err(AppError::internal)?,
        description_revision: row
            .try_get("description_revision")
            .map_err(AppError::internal)?,
        parameters: parameters
            .map(serde_json::from_value)
            .transpose()
            .map_err(AppError::internal)?,
        notes: row.try_get("notes").map_err(AppError::internal)?,
        personal_revision: row
            .try_get("personal_revision")
            .map_err(AppError::internal)?,
    })
}

async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    shared: bool,
) -> Result<(), AppError> {
    let authorization = permissions::lock(tx, actor).await?;
    if !authorization.has("words.access") {
        authorization.require("sentences.access")?;
    }
    if shared {
        authorization.require("speech.global_dubbing")?;
    }
    Ok(())
}

async fn lock_voice(tx: &mut Transaction<'_, Postgres>, alias: &str) -> Result<PgRow, AppError> {
    sqlx::query(
        r#"SELECT id, provider, provider_voice_id, locale, styles,
                  min_rate_percent, max_rate_percent, min_pitch_semitones, max_pitch_semitones
           FROM speech.voices WHERE alias = $1 AND enabled AND provider = 'azure'
           AND locale IN ('en-GB', 'en-US') FOR UPDATE"#,
    )
    .bind(alias)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| {
        AppError::not_found_with_code(ErrorCode::SpeechVoiceNotFound, "voice not available")
    })
}

fn validate_parameters(row: &PgRow, parameters: &VoiceParameters) -> Result<(), AppError> {
    if parameters.rate_percent < row.get::<i16, _>("min_rate_percent")
        || parameters.rate_percent > row.get::<i16, _>("max_rate_percent")
        || i16::from(parameters.pitch_semitones) < row.get::<i16, _>("min_pitch_semitones")
        || i16::from(parameters.pitch_semitones) > row.get::<i16, _>("max_pitch_semitones")
    {
        return Err(invalid());
    }
    let styles: Vec<String> =
        serde_json::from_value(row.get("styles")).map_err(AppError::internal)?;
    let voice = Voice::new(
        row.get::<String, _>("provider"),
        row.get::<String, _>("provider_voice_id"),
        row.get::<String, _>("locale"),
        styles,
    )
    .map_err(AppError::internal)?;
    SpeechOptions::new(
        &voice,
        parameters.style.clone(),
        parameters.rate_percent,
        parameters.pitch_semitones,
    )
    .map_err(|_| invalid())?;
    Ok(())
}

impl VoiceLibraryRepository {
    pub async fn groups(&self, actor: Uuid) -> Result<DubbingGroupsResponse, AppError> {
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize(&mut tx, actor, false).await?;
        let row = sqlx::query("SELECT groups, revision FROM speech.dubbing_groups WHERE singleton")
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        let result = DubbingGroupsResponse {
            groups: serde_json::from_value(row.get("groups")).map_err(AppError::internal)?,
            revision: row.get("revision"),
        };
        tx.commit().await.map_err(AppError::internal)?;
        Ok(result)
    }

    pub async fn save_groups(
        &self,
        actor: Uuid,
        request: SaveDubbingGroupsRequest,
    ) -> Result<DubbingGroupsResponse, AppError> {
        use std::collections::BTreeSet;
        if request.base_revision < 0 || request.groups.len() > 5 {
            return Err(invalid());
        }
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize(&mut tx, actor, true).await?;
        let revision: i64 = sqlx::query_scalar(
            "SELECT revision FROM speech.dubbing_groups WHERE singleton FOR UPDATE",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        if revision != request.base_revision {
            return Err(conflict());
        }
        let mut areas = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for group in &request.groups {
            if group.name.trim().is_empty()
                || group.name.chars().count() > 30
                || group.areas.is_empty()
                || !ids.insert(group.id)
                || !names.insert(group.name.trim())
            {
                return Err(invalid());
            }
            for area in &group.areas {
                if !["word", "azure", "grammar", "meaning", "example"].contains(&area.as_str())
                    || !areas.insert(area)
                {
                    return Err(invalid());
                }
            }
            for (locale, recommendations) in [("en-GB", &group.uk), ("en-US", &group.us)] {
                if recommendations.is_empty() || recommendations.len() > 20 {
                    return Err(invalid());
                }
                for recommendation in recommendations {
                    if recommendation.name.trim().is_empty()
                        || recommendation.name.chars().count() > 40
                        || !ids.insert(recommendation.id)
                    {
                        return Err(invalid());
                    }
                    let voice = lock_voice(&mut tx, &recommendation.voice_alias).await?;
                    if voice.get::<String, _>("locale") != locale {
                        return Err(invalid());
                    }
                    validate_parameters(&voice, &recommendation.parameters)?;
                }
            }
        }
        sqlx::query("UPDATE speech.dubbing_groups SET groups=$1, revision=revision+1, updated_by=$2, updated_at=now() WHERE singleton")
            .bind(serde_json::to_value(&request.groups).map_err(AppError::internal)?).bind(actor)
            .execute(&mut *tx).await.map_err(AppError::internal)?;
        tx.commit().await.map_err(AppError::internal)?;
        Ok(DubbingGroupsResponse {
            groups: request.groups,
            revision: revision + 1,
        })
    }

    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list(&self, actor: Uuid) -> Result<VoiceLibraryResponse, AppError> {
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize(&mut tx, actor, false).await?;
        let items = sqlx::query(ITEMS_SQL)
            .bind(actor)
            .bind(Option::<&str>::None)
            .fetch_all(&mut *tx)
            .await
            .map_err(AppError::internal)?
            .into_iter()
            .map(item)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(AppError::internal)?;
        Ok(VoiceLibraryResponse { items })
    }

    pub async fn save_settings(
        &self,
        actor: Uuid,
        alias: &str,
        request: SaveVoiceSettingsRequest,
    ) -> Result<VoiceLibraryItem, AppError> {
        if request.base_revision < 0 || request.notes.chars().count() > 3000 {
            return Err(invalid());
        }
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize(&mut tx, actor, false).await?;
        let voice = lock_voice(&mut tx, alias).await?;
        if let Some(parameters) = &request.parameters {
            validate_parameters(&voice, parameters)?;
        }
        let id: Uuid = voice.get("id");
        let revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM speech.admin_voice_settings WHERE admin_id=$1 AND voice_id=$2",
        )
        .bind(actor)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        if revision.unwrap_or(0) != request.base_revision {
            return Err(conflict());
        }
        sqlx::query(
            r#"INSERT INTO speech.admin_voice_settings(admin_id, voice_id, parameters, notes)
            VALUES ($1,$2,$3,$4) ON CONFLICT(admin_id, voice_id) DO UPDATE
            SET parameters=EXCLUDED.parameters, notes=EXCLUDED.notes,
                revision=speech.admin_voice_settings.revision+1, updated_at=now()"#,
        )
        .bind(actor)
        .bind(id)
        .bind(
            request
                .parameters
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(AppError::internal)?,
        )
        .bind(request.notes)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        let result = item(
            sqlx::query(ITEMS_SQL)
                .bind(actor)
                .bind(alias)
                .fetch_one(&mut *tx)
                .await
                .map_err(AppError::internal)?,
        )?;
        tx.commit().await.map_err(AppError::internal)?;
        Ok(result)
    }

    pub async fn save_description(
        &self,
        actor: Uuid,
        alias: &str,
        request: SaveVoiceDescriptionRequest,
    ) -> Result<VoiceLibraryItem, AppError> {
        if request.base_revision < 0 || request.description_zh.chars().count() > 3000 {
            return Err(invalid());
        }
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize(&mut tx, actor, true).await?;
        let id: Uuid = lock_voice(&mut tx, alias).await?.get("id");
        let revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM speech.voice_descriptions WHERE voice_id=$1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(AppError::internal)?;
        if revision.unwrap_or(0) != request.base_revision {
            return Err(conflict());
        }
        sqlx::query(
            r#"INSERT INTO speech.voice_descriptions(voice_id, description_zh, updated_by)
            VALUES ($1,$2,$3) ON CONFLICT(voice_id) DO UPDATE
            SET description_zh=EXCLUDED.description_zh, updated_by=EXCLUDED.updated_by,
                revision=speech.voice_descriptions.revision+1, updated_at=now()"#,
        )
        .bind(id)
        .bind(request.description_zh)
        .bind(actor)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        let result = item(
            sqlx::query(ITEMS_SQL)
                .bind(actor)
                .bind(alias)
                .fetch_one(&mut *tx)
                .await
                .map_err(AppError::internal)?,
        )?;
        tx.commit().await.map_err(AppError::internal)?;
        Ok(result)
    }
}
