use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::{
    admin::permissions,
    error::{AppError, ErrorCode},
    speech::{SpeechOptions, Voice},
};

use super::dto::{
    DeleteDubbingGroupRequest, DubbingGroup, DubbingGroupsReadResponse, DubbingGroupsResponse,
    DubbingGroupsV3Response, RestoreVoiceDescriptionRequest, SaveDubbingGroupOrderRequest,
    SaveDubbingGroupRequest, SaveDubbingGroupsRequest, SavePairedDubbingGroupRequest,
    SaveVoiceAccentRequest, SaveVoiceDescriptionRequest, SaveVoiceSettingsRequest,
    VoiceLibraryItem, VoiceLibraryResponse, VoiceParameters,
};

const ITEMS_SQL: &str = r#"
    SELECT v.alias, v.accent, v.accent_revision, d.description_zh, COALESCE(d.revision, 0) AS description_revision,
           s.parameters, COALESCE(s.notes, '') AS notes, COALESCE(s.revision, 0) AS personal_revision
    FROM speech.voices v
    LEFT JOIN speech.voice_descriptions d ON d.voice_id = v.id
    LEFT JOIN speech.admin_voice_settings s ON s.voice_id = v.id AND s.admin_id = $1
    WHERE v.enabled AND v.provider = 'azure'
      AND ($2::text IS NULL OR v.alias = $2)
    ORDER BY v.alias
"#;

pub fn global_group_id() -> Uuid {
    Uuid::from_u128(0x00000000000040008000000000000001)
}

async fn write_groups(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    groups: &[DubbingGroup],
) -> Result<(), AppError> {
    sqlx::query("UPDATE speech.dubbing_groups SET groups=$1, revision=revision+1, updated_by=$2, updated_at=now() WHERE singleton")
        .bind(serde_json::to_value(groups).map_err(AppError::internal)?).bind(actor)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}

pub struct VoiceLibraryRepository {
    pool: PgPool,
    test_run: Option<String>,
}

fn invalid() -> AppError {
    AppError::bad_request(
        ErrorCode::InvalidRequestBody,
        "invalid voice library settings",
    )
}

fn paired_client_required() -> AppError {
    AppError::conflict(
        ErrorCode::RevisionConflict,
        None,
        "此组已采用英美配对，请更新客户端后编辑，原配置已保留",
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
        accent: serde_json::from_value(serde_json::json!(row.get::<String, _>("accent")))
            .map_err(AppError::internal)?,
        accent_revision: row.get("accent_revision"),
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
    if !authorization.has("words.access") && !authorization.has("speech.global_dubbing") {
        authorization.require("sentences.access")?;
    }
    if shared {
        authorization.require("speech.global_dubbing")?;
    }
    Ok(())
}

async fn authorize_write(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    security_version: i64,
    shared: bool,
) -> Result<(), AppError> {
    authorize(tx, actor, shared).await?;
    let current: i64 = sqlx::query_scalar("SELECT security_version FROM admins WHERE id=$1")
        .bind(actor)
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    if current != security_version {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    Ok(())
}

async fn lock_voice(tx: &mut Transaction<'_, Postgres>, alias: &str) -> Result<PgRow, AppError> {
    sqlx::query(
        r#"SELECT id, provider, provider_voice_id, locale, styles,
                  min_rate_percent, max_rate_percent, min_pitch_semitones, max_pitch_semitones, accent, accent_revision
           FROM speech.voices WHERE alias = $1 AND enabled AND provider = 'azure'
           FOR UPDATE"#,
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
        parameters.volume_percent.unwrap_or(100),
    )
    .and_then(|options| {
        options.with_expression(&voice, parameters.style_degree, parameters.role.clone())
    })
    .map_err(|error| AppError::bad_request(ErrorCode::InvalidRequestBody, error.to_string()))?;
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

    pub async fn groups_v3(&self, actor: Uuid) -> Result<DubbingGroupsV3Response, AppError> {
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize(&mut tx, actor, false).await?;
        let result = super::pairs::read(&mut tx).await?;
        tx.commit().await.map_err(AppError::internal)?;
        Ok(result)
    }

    pub async fn save_order(
        &self,
        actor: Uuid,
        security_version: i64,
        request: SaveDubbingGroupOrderRequest,
    ) -> Result<DubbingGroupsV3Response, AppError> {
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize_write(&mut tx, actor, security_version, true).await?;
        let row = sqlx::query(
            "SELECT groups,order_revision FROM speech.dubbing_groups WHERE singleton FOR UPDATE",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        if request.base_revision < 0 || row.get::<i64, _>("order_revision") != request.base_revision
        {
            return Err(conflict());
        }
        let groups: Vec<DubbingGroup> =
            serde_json::from_value(row.get("groups")).map_err(AppError::internal)?;
        let ids = request
            .group_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        if ids.len() != groups.len()
            || ids.len() != request.group_ids.len()
            || groups.iter().any(|group| !ids.contains(&group.id))
            || request.group_ids.last() != Some(&global_group_id())
        {
            return Err(invalid());
        }
        let ordered = request
            .group_ids
            .iter()
            .map(|id| {
                groups
                    .iter()
                    .find(|group| group.id == *id)
                    .expect("validated identity")
                    .clone()
            })
            .collect::<Vec<_>>();
        if groups
            .iter()
            .map(|group| group.id)
            .ne(ordered.iter().map(|group| group.id))
        {
            let areas = groups
                .iter()
                .zip(&ordered)
                .filter(|(before, after)| before.id != after.id)
                .flat_map(|(before, after)| before.areas.iter().chain(&after.areas))
                .cloned()
                .collect();
            super::super::audio_updates::enqueue_areas(
                &mut tx,
                actor,
                security_version,
                (global_group_id(), request.base_revision + 1, "分组优先级"),
                areas,
                self.test_run.as_deref(),
            )
            .await?;
            sqlx::query(
                "UPDATE speech.dubbing_groups SET order_revision=order_revision+1 WHERE singleton",
            )
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
            write_groups(&mut tx, actor, &ordered).await?;
        }
        let result = super::pairs::read(&mut tx).await?;
        tx.commit().await.map_err(AppError::internal)?;
        Ok(result)
    }

    pub async fn save_paired_group(
        &self,
        actor: Uuid,
        security_version: i64,
        request: SavePairedDubbingGroupRequest,
        create: bool,
    ) -> Result<DubbingGroupsV3Response, AppError> {
        let (request, links) = super::pairs::into_legacy(request)?;
        match self
            .save_group_inner(actor, security_version, request, create, Some(links))
            .await?
        {
            DubbingGroupsReadResponse::Paired(result) => Ok(result),
            DubbingGroupsReadResponse::Legacy(_) => {
                unreachable!("paired write returns paired snapshot")
            }
        }
    }

    pub async fn save_groups(
        &self,
        actor: Uuid,
        security_version: i64,
        _request: SaveDubbingGroupsRequest,
    ) -> Result<DubbingGroupsResponse, AppError> {
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize_write(&mut tx, actor, security_version, true).await?;
        Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "分组已升级为单组保存，请保留输入并刷新页面",
        ))
    }

    pub async fn save_group(
        &self,
        actor: Uuid,
        security_version: i64,
        request: SaveDubbingGroupRequest,
        create: bool,
    ) -> Result<DubbingGroupsResponse, AppError> {
        match self
            .save_group_inner(actor, security_version, request, create, None)
            .await?
        {
            DubbingGroupsReadResponse::Legacy(result) => Ok(result),
            DubbingGroupsReadResponse::Paired(_) => {
                unreachable!("legacy write returns legacy snapshot")
            }
        }
    }

    async fn save_group_inner(
        &self,
        actor: Uuid,
        security_version: i64,
        mut request: SaveDubbingGroupRequest,
        create: bool,
        pairings: Option<Vec<super::pairs::PairLink>>,
    ) -> Result<DubbingGroupsReadResponse, AppError> {
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize_write(&mut tx, actor, security_version, true).await?;
        let row = sqlx::query(
            "SELECT groups, revision FROM speech.dubbing_groups WHERE singleton FOR UPDATE",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        let mut groups: Vec<DubbingGroup> =
            serde_json::from_value(row.get("groups")).map_err(AppError::internal)?;
        let index = groups.iter().position(|group| group.id == request.group.id);
        let paired: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM speech.dubbing_group_pairings WHERE group_id=$1)",
        )
        .bind(request.group.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        if paired && pairings.is_none() {
            return Err(paired_client_required());
        }
        if !paired
            && pairings.is_some()
            && let Some(index) = index
        {
            // Explicit conversion must retain every existing side identity, including unselected entries.
            for (old, new) in [
                (&groups[index].uk, &request.group.uk),
                (&groups[index].us, &request.group.us),
            ] {
                if old
                    .iter()
                    .any(|item| !new.iter().any(|next| next.id == item.id))
                {
                    return Err(AppError::bad_request(
                        ErrorCode::InvalidRequestBody,
                        "转换配对须保留所有历史推荐身份，请明确补齐配对",
                    ));
                }
            }
        }

        super::pairs::validate_identities(&mut tx, &groups, &request.group, pairings.as_deref())
            .await?;

        if create
            && groups
                .iter()
                .flat_map(|group| group.uk.iter().chain(&group.us))
                .any(|item| item.id == request.group.id)
        {
            return Err(invalid());
        }
        if !create && index.is_none() {
            return Err(AppError::not_found("分组不存在"));
        }
        if create != index.is_none() || request.base_revision < 0 {
            return Err(conflict());
        }
        if let Some(index) = index {
            if groups[index].revision != request.base_revision {
                return Err(conflict());
            }
            if groups[index].is_global != request.group.is_global
                || (groups[index].is_global
                    && (request.group.name != "全局常用" || !request.group.areas.is_empty()))
            {
                return Err(invalid());
            }
        } else if request.base_revision != 0
            || request.group.is_global
            || request.group.id.is_nil()
            || request.group.id == global_group_id()
        {
            return Err(invalid());
        }
        if request.group.name.trim().is_empty()
            || request.group.name.chars().count() > 30
            || request.group.name.contains('\0')
            || request.group.areas.len() > 5
        {
            return Err(invalid());
        }
        let mut assigned = std::collections::BTreeSet::new();
        for area in &request.group.areas {
            if !["word", "azure", "grammar", "meaning", "example"].contains(&area.as_str())
                || !assigned.insert(area)
            {
                return Err(invalid());
            }
        }
        for other in groups.iter().filter(|group| group.id != request.group.id) {
            if other.name.trim() == request.group.name.trim() {
                return Err(AppError::conflict(
                    ErrorCode::RevisionConflict,
                    None,
                    "分组名称已被其他组使用，请保留草稿并重新载入",
                ));
            }
        }
        if create && groups.len() >= 6 {
            return Err(invalid());
        }
        sqlx::query("SELECT pg_advisory_xact_lock(734892105)")
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        let mut ids = std::collections::BTreeSet::new();
        for (accent, items) in [("gb", &mut request.group.uk), ("us", &mut request.group.us)] {
            if items.len() > 20 {
                return Err(invalid());
            }
            for item in items.iter() {
                if item.id.is_nil()
                    || !ids.insert(item.id)
                    || item.name.trim().is_empty()
                    || item.name.chars().count() > 40
                    || item.name.contains('\0')
                    || item.id == request.group.id
                    || groups.iter().any(|group| group.id == item.id)
                    || groups
                        .iter()
                        .filter(|group| group.id != request.group.id)
                        .flat_map(|group| group.uk.iter().chain(&group.us))
                        .any(|other| other.id == item.id)
                {
                    return Err(invalid());
                }
                let voice = lock_voice(&mut tx, &item.voice_alias).await?;
                let stored = index.and_then(|i| {
                    if accent == "gb" {
                        groups[i].uk.iter().find(|old| old.id == item.id)
                    } else {
                        groups[i].us.iter().find(|old| old.id == item.id)
                    }
                });
                let unchanged = stored.is_some_and(|old| {
                    let mut retained = old.clone();
                    if !item.selected {
                        retained.selected = false;
                    }
                    serde_json::to_value(retained).ok() == serde_json::to_value(item).ok()
                });
                if voice.get::<String, _>("accent") != accent && !unchanged {
                    return Err(invalid());
                }
                if !unchanged {
                    if !crate::speech::capabilities::synthesis_available(
                        &voice.get::<String, _>("provider_voice_id"),
                        &voice.get::<String, _>("locale"),
                    ) {
                        return Err(invalid());
                    }
                    validate_parameters(&voice, &item.parameters)?;
                }
            }
            if pairings.is_none() {
                items.sort_by_key(|item| !item.selected);
            }
        }
        if let Some(links) = &pairings {
            sqlx::query("INSERT INTO speech.dubbing_group_pairings(group_id,pairs) VALUES($1,$2) ON CONFLICT(group_id) DO UPDATE SET pairs=EXCLUDED.pairs")
                .bind(request.group.id).bind(serde_json::to_value(links).map_err(AppError::internal)?)
                .execute(&mut *tx).await.map_err(AppError::internal)?;
        }
        if create {
            sqlx::query(
                "UPDATE speech.dubbing_groups SET order_revision=order_revision+1 WHERE singleton",
            )
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        }
        request.group.revision = index.map_or(1, |i| groups[i].revision + 1);
        request.group.name = request.group.name.trim().into();
        super::super::audio_updates::enqueue(
            &mut tx,
            actor,
            security_version,
            request.group.id,
            request.group.revision,
            (index.map(|i| &groups[i]), Some(&request.group)),
            self.test_run.as_deref(),
        )
        .await?;
        if let Some(index) = index {
            groups[index] = request.group;
        } else {
            let position = groups
                .iter()
                .position(|group| group.is_global)
                .unwrap_or(groups.len());
            groups.insert(position, request.group);
        }
        write_groups(&mut tx, actor, &groups).await?;
        let result = if pairings.is_some() {
            DubbingGroupsReadResponse::Paired(super::pairs::read(&mut tx).await?)
        } else {
            DubbingGroupsReadResponse::Legacy(DubbingGroupsResponse {
                groups,
                revision: row.get::<i64, _>("revision") + 1,
            })
        };
        tx.commit().await.map_err(AppError::internal)?;
        Ok(result)
    }

    pub async fn delete_group(
        &self,
        actor: Uuid,
        security_version: i64,
        id: Uuid,
        request: DeleteDubbingGroupRequest,
    ) -> Result<DubbingGroupsResponse, AppError> {
        match self
            .delete_group_inner(actor, security_version, id, request, false)
            .await?
        {
            DubbingGroupsReadResponse::Legacy(result) => Ok(result),
            DubbingGroupsReadResponse::Paired(_) => {
                unreachable!("legacy delete returns legacy snapshot")
            }
        }
    }

    pub async fn delete_paired_group(
        &self,
        actor: Uuid,
        security_version: i64,
        id: Uuid,
        request: DeleteDubbingGroupRequest,
    ) -> Result<DubbingGroupsV3Response, AppError> {
        match self
            .delete_group_inner(actor, security_version, id, request, true)
            .await?
        {
            DubbingGroupsReadResponse::Paired(result) => Ok(result),
            DubbingGroupsReadResponse::Legacy(_) => {
                unreachable!("paired delete returns paired snapshot")
            }
        }
    }

    async fn delete_group_inner(
        &self,
        actor: Uuid,
        security_version: i64,
        id: Uuid,
        request: DeleteDubbingGroupRequest,
        allow_paired: bool,
    ) -> Result<DubbingGroupsReadResponse, AppError> {
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize_write(&mut tx, actor, security_version, true).await?;
        let row = sqlx::query(
            "SELECT groups, revision FROM speech.dubbing_groups WHERE singleton FOR UPDATE",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        let mut groups: Vec<DubbingGroup> =
            serde_json::from_value(row.get("groups")).map_err(AppError::internal)?;
        let index = groups
            .iter()
            .position(|group| group.id == id)
            .ok_or_else(|| AppError::not_found("分组不存在"))?;
        if !allow_paired
            && sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM speech.dubbing_group_pairings WHERE group_id=$1)",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?
        {
            return Err(paired_client_required());
        }
        if groups[index].is_global {
            return Err(invalid());
        }
        if groups[index].revision != request.base_revision {
            return Err(conflict());
        }
        sqlx::query("SELECT pg_advisory_xact_lock(734892105)")
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        super::super::audio_updates::enqueue(
            &mut tx,
            actor,
            security_version,
            id,
            groups[index].revision + 1,
            (Some(&groups[index]), None),
            self.test_run.as_deref(),
        )
        .await?;
        sqlx::query("DELETE FROM speech.dubbing_group_pairings WHERE group_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        sqlx::query(
            "UPDATE speech.dubbing_groups SET order_revision=order_revision+1 WHERE singleton",
        )
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        groups.remove(index);
        write_groups(&mut tx, actor, &groups).await?;
        let result = if allow_paired {
            DubbingGroupsReadResponse::Paired(super::pairs::read(&mut tx).await?)
        } else {
            DubbingGroupsReadResponse::Legacy(DubbingGroupsResponse {
                groups,
                revision: row.get::<i64, _>("revision") + 1,
            })
        };
        tx.commit().await.map_err(AppError::internal)?;
        Ok(result)
    }

    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            test_run: None,
        }
    }

    pub(crate) fn with_test_run(mut self, test_run: Option<String>) -> Self {
        self.test_run = test_run;
        self
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
        security_version: i64,
        request: SaveVoiceSettingsRequest,
    ) -> Result<VoiceLibraryItem, AppError> {
        if request.base_revision < 0
            || request.notes.chars().count() > 3000
            || request.notes.contains('\0')
        {
            return Err(invalid());
        }
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize_write(&mut tx, actor, security_version, false).await?;
        let voice = lock_voice(&mut tx, alias).await?;
        let id: Uuid = voice.get("id");
        let current: Option<(i64, Option<serde_json::Value>)> = sqlx::query_as(
            "SELECT revision, parameters FROM speech.admin_voice_settings WHERE admin_id=$1 AND voice_id=$2",
        )
        .bind(actor)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        if current.as_ref().map(|value| value.0).unwrap_or(0) != request.base_revision {
            return Err(conflict());
        }
        if let Some(parameters) = &request.parameters {
            let incoming = serde_json::to_value(parameters).map_err(AppError::internal)?;
            if voice
                .get::<String, _>("provider_voice_id")
                .ends_with(":DragonHDFlashLatestNeural")
                || current.as_ref().and_then(|value| value.1.as_ref()) != Some(&incoming)
            {
                validate_parameters(&voice, parameters)?;
            }
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

    pub async fn save_accent(
        &self,
        actor: Uuid,
        alias: &str,
        security_version: i64,
        request: SaveVoiceAccentRequest,
    ) -> Result<VoiceLibraryItem, AppError> {
        if request.base_revision < 0 {
            return Err(invalid());
        }
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize_write(&mut tx, actor, security_version, true).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(734892105)")
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        let voice = lock_voice(&mut tx, alias).await?;
        if voice.get::<i64, _>("accent_revision") != request.base_revision {
            return Err(conflict());
        }
        sqlx::query("UPDATE speech.voices SET accent=$2, accent_revision=accent_revision+1, updated_at=now() WHERE id=$1")
            .bind(voice.get::<Uuid, _>("id"))
            .bind(request.accent.as_str())
            .execute(&mut *tx).await.map_err(AppError::internal)?;
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
        security_version: i64,
        request: SaveVoiceDescriptionRequest,
    ) -> Result<VoiceLibraryItem, AppError> {
        if request.base_revision < 0
            || request.description_zh.chars().count() > 3000
            || request.description_zh.contains('\0')
        {
            return Err(invalid());
        }
        self.save_description_value(
            actor,
            alias,
            security_version,
            Some(request.description_zh),
            request.base_revision,
        )
        .await
    }

    pub async fn restore_description(
        &self,
        actor: Uuid,
        alias: &str,
        security_version: i64,
        request: RestoreVoiceDescriptionRequest,
    ) -> Result<VoiceLibraryItem, AppError> {
        if request.base_revision < 0 {
            return Err(invalid());
        }
        self.save_description_value(actor, alias, security_version, None, request.base_revision)
            .await
    }

    async fn save_description_value(
        &self,
        actor: Uuid,
        alias: &str,
        security_version: i64,
        description: Option<String>,
        base_revision: i64,
    ) -> Result<VoiceLibraryItem, AppError> {
        let mut tx = self.pool.begin().await.map_err(AppError::internal)?;
        authorize_write(&mut tx, actor, security_version, true).await?;
        let id: Uuid = lock_voice(&mut tx, alias).await?.get("id");
        let revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM speech.voice_descriptions WHERE voice_id=$1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(AppError::internal)?;
        if revision.unwrap_or(0) != base_revision {
            return Err(conflict());
        }
        sqlx::query(
            r#"INSERT INTO speech.voice_descriptions(voice_id, description_zh, updated_by)
            VALUES ($1,$2,$3) ON CONFLICT(voice_id) DO UPDATE
            SET description_zh=EXCLUDED.description_zh, updated_by=EXCLUDED.updated_by,
                revision=speech.voice_descriptions.revision+1, updated_at=now()"#,
        )
        .bind(id)
        .bind(description)
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
