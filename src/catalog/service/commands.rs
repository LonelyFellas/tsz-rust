use super::*;

// --- part commands ---

impl CatalogService {
    pub async fn create_part(
        &self,
        actor_id: Uuid,
        request: CreatePartRequest,
    ) -> Result<PartOfSpeechConfig, CatalogServiceError> {
        let value = NewPart {
            id: Uuid::now_v7(),
            code: valid_part_code(request.code)?,
            name_zh: normalized_text(request.name_zh, "name_zh", 64)?,
            name_en: normalized_text(request.name_en, "name_en", 64)?,
            abbreviation: normalized_text(request.abbreviation, "abbreviation", 16)?,
            sort_order: request.sort_order,
            actor_id,
        };

        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        CatalogRepository::insert_part(&mut tx, &value)
            .await
            .map_err(map_repository_error)?;
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        let response = CatalogRepository::part_by_id(&mut tx, value.id)
            .await
            .map_err(map_repository_error)?
            .ok_or_else(invariant_missing_part)?
            .into();
        tx.commit().await.map_err(database_error)?;
        Ok(response)
    }

    pub async fn update_part(
        &self,
        actor_id: Uuid,
        id: Uuid,
        request: UpdatePartRequest,
    ) -> Result<PartOfSpeechConfig, CatalogServiceError> {
        validate_revision(request.base_revision)?;
        let changes = PartChanges {
            name_zh: normalized_text(request.name_zh, "name_zh", 64)?,
            name_en: normalized_text(request.name_en, "name_en", 64)?,
            abbreviation: normalized_text(request.abbreviation, "abbreviation", 16)?,
            sort_order: request.sort_order,
        };
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let updated =
            CatalogRepository::update_part(&mut tx, id, request.base_revision, actor_id, &changes)
                .await
                .map_err(map_repository_error)?;
        if !updated {
            return Err(revision_or_part_not_found(&mut tx, id, request.base_revision).await?);
        }
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        let response = CatalogRepository::part_by_id(&mut tx, id)
            .await
            .map_err(map_repository_error)?
            .ok_or_else(invariant_missing_part)?
            .into();
        tx.commit().await.map_err(database_error)?;
        Ok(response)
    }

    pub async fn delete_part(
        &self,
        id: Uuid,
        base_revision: i64,
    ) -> Result<(), CatalogServiceError> {
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let Some((current_revision, code)) = CatalogRepository::part_revision(&mut tx, id, true)
            .await
            .map_err(map_repository_error)?
        else {
            return Err(CatalogServiceError::PartNotFound);
        };
        if current_revision != base_revision {
            return Err(CatalogServiceError::RevisionConflict {
                current_revision,
                part_of_speech_id: id,
                code,
            });
        }
        let usage_count = CatalogRepository::part_usage_count(&mut tx, id)
            .await
            .map_err(map_repository_error)?;
        if usage_count > 0 {
            return Err(CatalogServiceError::PartInUse {
                usage_count: Some(usage_count),
            });
        }
        CatalogRepository::delete_part(&mut tx, id)
            .await
            .map_err(map_repository_error)?;
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(())
    }
}

// --- sub part commands ---

impl CatalogService {
    pub async fn create_sub_part(
        &self,
        actor_id: Uuid,
        part_id: Uuid,
        request: CreateSubPartRequest,
    ) -> Result<SubPartOfSpeechConfig, CatalogServiceError> {
        let value = NewSubPart {
            id: Uuid::now_v7(),
            part_of_speech_id: part_id,
            code: valid_sub_part_code(request.code)?,
            name_zh: normalized_text(request.name_zh, "name_zh", 64)?,
            name_en: normalized_text(request.name_en, "name_en", 64)?,
            sort_order: request.sort_order,
            actor_id,
        };
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        if CatalogRepository::part_revision(&mut tx, part_id, false)
            .await
            .map_err(map_repository_error)?
            .is_none()
        {
            return Err(CatalogServiceError::PartNotFound);
        }
        CatalogRepository::insert_sub_part(&mut tx, &value)
            .await
            .map_err(map_repository_error)?;
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        let response = CatalogRepository::sub_part_by_id(&mut tx, part_id, value.id)
            .await
            .map_err(map_repository_error)?
            .ok_or_else(invariant_missing_sub_part)?
            .into();
        tx.commit().await.map_err(database_error)?;
        Ok(response)
    }

    pub async fn update_sub_part(
        &self,
        actor_id: Uuid,
        part_id: Uuid,
        sub_id: Uuid,
        request: UpdateSubPartRequest,
    ) -> Result<SubPartOfSpeechConfig, CatalogServiceError> {
        validate_revision(request.base_revision)?;
        let changes = SubPartChanges {
            name_zh: normalized_text(request.name_zh, "name_zh", 64)?,
            name_en: normalized_text(request.name_en, "name_en", 64)?,
            sort_order: request.sort_order,
        };
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let updated = CatalogRepository::update_sub_part(
            &mut tx,
            part_id,
            sub_id,
            request.base_revision,
            actor_id,
            &changes,
        )
        .await
        .map_err(map_repository_error)?;
        if !updated {
            return Err(revision_or_sub_part_not_found(
                &mut tx,
                part_id,
                sub_id,
                request.base_revision,
            )
            .await?);
        }
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        let response = CatalogRepository::sub_part_by_id(&mut tx, part_id, sub_id)
            .await
            .map_err(map_repository_error)?
            .ok_or_else(invariant_missing_sub_part)?
            .into();
        tx.commit().await.map_err(database_error)?;
        Ok(response)
    }

    pub async fn delete_sub_part(
        &self,
        part_id: Uuid,
        sub_id: Uuid,
        base_revision: i64,
    ) -> Result<(), CatalogServiceError> {
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let Some((current_revision, code)) =
            CatalogRepository::sub_part_revision(&mut tx, part_id, sub_id, true)
                .await
                .map_err(map_repository_error)?
        else {
            return Err(CatalogServiceError::SubPartNotFound);
        };
        if current_revision != base_revision {
            return Err(CatalogServiceError::RevisionConflict {
                current_revision,
                part_of_speech_id: part_id,
                code,
            });
        }
        let usage_count = CatalogRepository::sub_part_usage_count(&mut tx, sub_id)
            .await
            .map_err(map_repository_error)?;
        if usage_count > 0 {
            return Err(CatalogServiceError::SubPartInUse {
                usage_count: Some(usage_count),
            });
        }
        CatalogRepository::delete_sub_part(&mut tx, part_id, sub_id)
            .await
            .map_err(map_repository_error)?;
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(())
    }
}
