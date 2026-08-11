use super::*;

// --- query ---

impl LexiconService {
    pub async fn get(&self, id: Uuid) -> Result<AdminWordV2Envelope, LexiconServiceError> {
        let record = self
            .repository
            .entry_by_id(id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        Ok(AdminWordV2Envelope {
            word: entry_from_record(record)?,
        })
    }

    pub async fn related_search(
        &self,
        query: RelatedSearchQuery,
    ) -> Result<RelatedSearchResponse, LexiconServiceError> {
        let limit = query.limit.unwrap_or(20);
        if !(1..=100).contains(&limit) {
            return Err(LexiconServiceError::InvalidField {
                field: "limit",
                message: "limit must be between 1 and 100",
            });
        }
        let q = query.q.unwrap_or_default();
        if q.contains('\0') {
            return Err(LexiconServiceError::InvalidField {
                field: "q",
                message: "q must not contain NUL characters",
            });
        }
        let q = q.trim();
        if q.is_empty() {
            return Ok(RelatedSearchResponse {
                results: Vec::new(),
            });
        }
        let records = self
            .repository
            .related_search(q, query.kind, i64::from(limit))
            .await
            .map_err(repository_error)?;
        let results = records
            .into_iter()
            .map(|record| {
                let word: AdminWordV2 =
                    serde_json::from_value(record.snapshot).map_err(serialization_error)?;
                let senses = word
                    .meanings
                    .pos
                    .iter()
                    .flat_map(|pos| &pos.senses)
                    .map(|sense| RelatedWordSense {
                        sense_id: sense.id,
                        gloss: published_sense_gloss(sense),
                    })
                    .collect();
                Ok(RelatedWordResult {
                    word_id: word.id,
                    headword: published_word_headword(&word),
                    kind: word.kind,
                    senses,
                })
            })
            .collect::<Result<Vec<_>, LexiconServiceError>>()?;
        Ok(RelatedSearchResponse { results })
    }

    pub async fn list(
        &self,
        query: AdminWordListQuery,
    ) -> Result<AdminWordListResponse, LexiconServiceError> {
        let page = query.page.unwrap_or(1);
        let page_size = query.page_size.unwrap_or(20);
        if page == 0 {
            return Err(LexiconServiceError::InvalidField {
                field: "page",
                message: "page must be at least 1",
            });
        }
        if !(1..=100).contains(&page_size) {
            return Err(LexiconServiceError::InvalidField {
                field: "page_size",
                message: "page_size must be between 1 and 100",
            });
        }
        if let Some(level) = query.level.as_deref()
            && !valid_level(level)
        {
            return Err(LexiconServiceError::InvalidField {
                field: "level",
                message: "invalid CEFR level",
            });
        }
        if let (Some(from), Some(to)) = (query.created_from, query.created_to)
            && from >= to
        {
            return Err(LexiconServiceError::InvalidField {
                field: "created_to",
                message: "created_to must be later than created_from",
            });
        }
        for (field, value) in [
            ("q", query.q.as_deref()),
            ("gloss", query.gloss.as_deref()),
            ("pos", query.pos.as_deref()),
        ] {
            if value.is_some_and(|value| value.contains('\0')) {
                return Err(LexiconServiceError::InvalidField {
                    field,
                    message: "query text must not contain NUL characters",
                });
            }
        }

        let filter = ListFilter {
            q: trimmed(query.q),
            gloss: trimmed(query.gloss),
            kind: query.kind.map(kind_string_owned),
            pos: trimmed(query.pos),
            level: query.level,
            status: query.status.map(status_string_owned),
            created_from: query.created_from,
            created_to: query.created_to,
            limit: page_size as i64,
            offset: (i64::from(page) - 1) * i64::from(page_size),
        };
        let records = self
            .repository
            .list(&filter)
            .await
            .map_err(repository_error)?;
        let total = if let Some(record) = records.first() {
            record.total
        } else {
            self.repository
                .list_total(&filter)
                .await
                .map_err(repository_error)?
        };
        let words = records
            .into_iter()
            .map(|record| AdminWordListItem {
                schema_version: 2,
                id: record.id,
                headword: record.headword,
                kind: parse_kind(&record.kind).unwrap_or(EntryKind::Word),
                revision: record.revision,
                lifecycle_revision: record.lifecycle_revision,
                gloss: record.gloss,
                pos_list: record.pos_list,
                levels: record.levels,
                status: if record.is_archived {
                    AdminWordStatus::Archived
                } else if record.is_published {
                    AdminWordStatus::Published
                } else {
                    AdminWordStatus::Draft
                },
                published_revision: record.published_revision,
                has_unpublished_changes: record.has_unpublished_changes,
                max_reachable_step: max_reachable_step(&record.completed_steps),
                created_by_name: record.created_by_name,
                created_at: record.created_at,
                updated_at: record.updated_at,
            })
            .collect();
        Ok(AdminWordListResponse {
            words,
            page: AdminWordListPage {
                page,
                page_size,
                total,
            },
        })
    }

    pub async fn stats(&self) -> Result<AdminWordStats, LexiconServiceError> {
        let record = self.repository.stats().await.map_err(repository_error)?;
        Ok(AdminWordStats {
            total: record.total,
            today: record.today,
            month: record.month,
        })
    }
}

// --- dialect suggestion ---

impl LexiconService {
    pub async fn suggest_dialect_variants(
        &self,
        input: SuggestDialectVariantsInputV2,
    ) -> Result<SuggestDialectVariantsResponseV2, LexiconServiceError> {
        validate_request(&input)?;

        let keys = evidence_keys(&input.items);
        let surfaces = self
            .repository
            .region_surfaces(&keys)
            .await
            .map_err(repository_error)?;
        let source_surfaces = surfaces
            .into_iter()
            .filter(|surface| {
                family_matches_source_dialect(&surface.region_family, input.source_dialect)
            })
            .collect::<Vec<_>>();
        let target_keys = source_surfaces
            .iter()
            .flat_map(|surface| &surface.targets)
            .filter_map(|target| normalize_headword(target).ok().map(|value| value.key))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut candidates = self
            .repository
            .dictionary_candidates(&target_keys)
            .await
            .map_err(repository_error)?;
        candidates.sort_by(|left, right| {
            left.normalized_term
                .cmp(&right.normalized_term)
                .then_with(|| left.term.cmp(&right.term))
        });
        let candidate_by_key = candidates
            .into_iter()
            .filter(|candidate| {
                family_matches_source_dialect(&candidate.region_family, input.target_dialect)
            })
            .map(|candidate| (candidate.normalized_term, candidate.term))
            .collect::<HashMap<_, _>>();
        let mut replacements = HashMap::new();
        for surface in source_surfaces {
            let counterpart = surface.targets.iter().find_map(|target| {
                normalize_headword(target)
                    .ok()
                    .and_then(|normalized| candidate_by_key.get(&normalized.key))
            });
            if let Some(counterpart) = counterpart {
                replacements.insert(surface.normalized_term, counterpart.clone());
            }
        }

        let provider = DictionaryRegionRulesProvider;
        let suggestions = input
            .items
            .iter()
            .filter_map(|item| provider.suggest(item, &replacements))
            .collect();
        Ok(SuggestDialectVariantsResponseV2 {
            provider: DialectSuggestionProviderV2 {
                kind: provider.kind().to_owned(),
                version: provider.version().to_owned(),
            },
            suggestions,
        })
    }
}

fn validate_request(input: &SuggestDialectVariantsInputV2) -> Result<(), LexiconServiceError> {
    if input.source_dialect == input.target_dialect {
        return Err(semantic(
            "target_dialect",
            "target_dialect must differ from source_dialect",
        ));
    }
    if input.items.is_empty() || input.items.len() > 100 {
        return Err(semantic(
            "items",
            "items must contain between 1 and 100 values",
        ));
    }
    let mut client_ids = std::collections::HashSet::new();
    for item in &input.items {
        let client_id = item.client_id();
        if client_id.trim().is_empty() || client_id.chars().count() > 100 {
            return Err(semantic(
                "items.client_id",
                "client_id must contain between 1 and 100 characters",
            ));
        }
        if !client_ids.insert(client_id) {
            return Err(semantic(
                "items.client_id",
                "client_id must be unique within one request",
            ));
        }
        match item {
            DialectVariantSuggestionItemV2::Form {
                field_kind, value, ..
            } => {
                if *field_kind != DialectSuggestionFieldKind::Form {
                    return Err(semantic(
                        "items.field_kind",
                        "string values require field_kind form",
                    ));
                }
                if value.trim().is_empty() || value.chars().count() > 200 {
                    return Err(semantic(
                        "items.value",
                        "form values must contain between 1 and 200 characters",
                    ));
                }
                normalize_headword(value)
                    .map_err(|_| semantic("items.value", "form value is not valid text"))?;
            }
            DialectVariantSuggestionItemV2::RichText {
                field_kind, value, ..
            } => {
                if !matches!(
                    field_kind,
                    DialectSuggestionFieldKind::Definition | DialectSuggestionFieldKind::Example
                ) {
                    return Err(semantic(
                        "items.field_kind",
                        "rich text values require definition or example field_kind",
                    ));
                }
                let mut canonical = value.clone();
                if crate::lexicon::rich_text::canonicalize(&mut canonical).is_err() {
                    return Err(semantic("items.value", "rich text value is invalid"));
                }
            }
        }
    }
    Ok(())
}

fn semantic(field: &'static str, message: &'static str) -> LexiconServiceError {
    LexiconServiceError::UnprocessableField { field, message }
}

fn family_matches_source_dialect(family: &str, dialect: SourceDialect) -> bool {
    matches!(
        (family_dialect(family), dialect),
        (Some(Dialect::Uk), SourceDialect::Uk) | (Some(Dialect::Us), SourceDialect::Us)
    )
}
