use super::*;

impl CatalogService {
    pub async fn catalog(&self) -> Result<CatalogResponse, CatalogServiceError> {
        let records = self
            .repository
            .catalog()
            .await
            .map_err(map_repository_error)?;
        let catalog_version = records
            .first()
            .map(|record| record.catalog_version)
            .ok_or_else(|| {
                CatalogServiceError::Repository(CatalogRepositoryError::Invariant(
                    "metadata row is missing",
                ))
            })?;

        let form_types = records
            .first()
            .map(|r| r.form_types.0.clone())
            .unwrap_or_default();

        let mut items: Vec<CatalogPart> = Vec::new();
        for record in records {
            let Some(part_id) = record.part_id else {
                continue;
            };
            if items.last().is_none_or(|item| item.id != part_id) {
                let code = required(record.part_code, "part code is null")?;
                let sub_pos_required = crate::catalog::rules::requires_sub_pos(&code);
                // 词形候选按词性收窄：原形对所有词性通用，此处不进候选（它是必有项）。
                let allowed_form_types = form_types
                    .iter()
                    .filter(|f| f.code != "base" && f.part_of_speech_id == Some(part_id))
                    .map(|f| f.code.clone())
                    .collect::<Vec<_>>();
                items.push(CatalogPart {
                    id: part_id,
                    code,
                    name_zh: required(record.part_name_zh, "part name_zh is null")?,
                    name_en: required(record.part_name_en, "part name_en is null")?,
                    abbreviation: required(record.part_abbreviation, "part abbreviation is null")?,
                    short_name_zh: required(
                        record.part_short_name_zh,
                        "part short_name_zh is null",
                    )?,
                    full_name_en: required(record.part_full_name_en, "part full_name_en is null")?,
                    sort_order: required(record.part_sort_order, "part sort_order is null")?,
                    default_form_types: allowed_form_types.clone(),
                    allowed_form_types,
                    sub_parts_extensible: true,
                    sub_pos_required,
                    sub_parts: Vec::new(),
                });
            }
            if let Some(sub_id) = record.sub_id {
                items
                    .last_mut()
                    .expect("part was inserted above")
                    .sub_parts
                    .push(CatalogSubPart {
                        id: sub_id,
                        code: required(record.sub_code, "sub part code is null")?,
                        name_zh: required(record.sub_name_zh, "sub part name_zh is null")?,
                        name_en: required(record.sub_name_en, "sub part name_en is null")?,
                        short_name_zh: required(
                            record.sub_short_name_zh,
                            "sub part short_name_zh is null",
                        )?,
                        abbreviation: required(
                            record.sub_abbreviation,
                            "sub part abbreviation is null",
                        )?,
                        full_name_en: required(
                            record.sub_full_name_en,
                            "sub part full_name_en is null",
                        )?,
                        sort_order: required(record.sub_sort_order, "sub part sort_order is null")?,
                    });
            }
        }
        Ok(CatalogResponse {
            form_types,
            catalog_version,
            items,
        })
    }

    pub async fn list_parts(
        &self,
        query: PartListQuery,
    ) -> Result<PartListResponse, CatalogServiceError> {
        let page = query.page.unwrap_or(1);
        let page_size = query.page_size.unwrap_or(10);
        if page == 0 {
            return Err(CatalogServiceError::InvalidQuery("page must be at least 1"));
        }
        if !(1..=100).contains(&page_size) {
            return Err(CatalogServiceError::InvalidQuery(
                "page_size must be between 1 and 100",
            ));
        }
        let q = query.q.and_then(|value| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        });
        let filter = PartListFilter { q, page, page_size };
        let (records, total) = self
            .repository
            .list_parts(&filter)
            .await
            .map_err(map_repository_error)?;
        Ok(PaginatedResponse {
            items: records.into_iter().map(Into::into).collect(),
            pagination: filter.pagination(total),
        })
    }

    pub async fn list_sub_parts(
        &self,
        part_id: Uuid,
    ) -> Result<SubPartListResponse, CatalogServiceError> {
        let Some(records) = self
            .repository
            .list_sub_parts(part_id)
            .await
            .map_err(map_repository_error)?
        else {
            return Err(CatalogServiceError::PartNotFound);
        };
        Ok(SubPartListResponse {
            items: records.into_iter().map(Into::into).collect(),
        })
    }
}
