use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

#[derive(Debug, Serialize, Deserialize)]
struct RelatedSearchCursor {
    actor_id: Uuid,
    q: String,
    kind: Option<EntryKind>,
    match_mode: RelatedSearchMatchMode,
    exclude_exact: bool,
    include_v3: bool,
    page_size: u32,
    total: u64,
    consumed: u64,
    last_kind: Option<EntryKind>,
    last_headword: Option<String>,
    last_word_id: Option<Uuid>,
    dataset_version: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct RelatedSearchCursorEnvelope {
    payload: String,
    signature: String,
}

fn encode_related_search_cursor(cursor: &RelatedSearchCursor, key: &[u8]) -> String {
    let payload = serde_json::to_vec(cursor).expect("cursor serialization cannot fail");
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts every key length");
    mac.update(&payload);
    let envelope = RelatedSearchCursorEnvelope {
        payload: URL_SAFE_NO_PAD.encode(&payload),
        signature: URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()),
    };
    URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&envelope).expect("cursor envelope serialization cannot fail"))
}

fn decode_related_search_cursor(encoded: &str, key: &[u8]) -> Result<RelatedSearchCursor, ()> {
    let envelope_bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| ())?;
    let envelope: RelatedSearchCursorEnvelope =
        serde_json::from_slice(&envelope_bytes).map_err(|_| ())?;
    let payload = URL_SAFE_NO_PAD.decode(envelope.payload).map_err(|_| ())?;
    let signature = URL_SAFE_NO_PAD.decode(envelope.signature).map_err(|_| ())?;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| ())?;
    mac.update(&payload);
    mac.verify_slice(&signature).map_err(|_| ())?;
    serde_json::from_slice(&payload).map_err(|_| ())
}

fn related_search_response(
    v2: bool,
    results: Vec<RelatedWordResultAny>,
    total: u64,
    next_cursor: Option<String>,
) -> RelatedSearchResponse {
    if v2 {
        RelatedSearchResponse::V2(RelatedSearchV2Response {
            results,
            total,
            next_cursor,
        })
    } else {
        RelatedSearchResponse::Legacy(RelatedSearchLegacyResponse { results })
    }
}

fn include_related_v3_match(
    matches: &mut Vec<RelatedWordMatchV3>,
    candidate: RelatedWordMatchV3,
    normalized_q: &str,
    match_mode: RelatedSearchMatchMode,
) -> Result<(), LexiconServiceError> {
    let normalized_spelling = normalize_headword(&candidate.spelling)
        .map_err(surface_projection_error)?
        .key;
    let is_match = match match_mode {
        RelatedSearchMatchMode::Exact => normalized_spelling == normalized_q,
        RelatedSearchMatchMode::Contains => normalized_spelling.contains(normalized_q),
    };
    if is_match {
        matches.push(candidate);
    }
    Ok(())
}

fn related_v3_matches(
    word: &AdminWordV3,
    normalized_q: &str,
    match_mode: RelatedSearchMatchMode,
) -> Result<Vec<RelatedWordMatchV3>, LexiconServiceError> {
    let mut matches = Vec::new();
    for pos in &word.forms.pos {
        for form in &pos.forms {
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => include_related_v3_match(
                    &mut matches,
                    RelatedWordMatchV3 {
                        pos_id: pos.pos_id,
                        form_id: form.id,
                        variant_id: common.id,
                        form_type: form.form_type,
                        dialect: Dialect::Common,
                        spelling: common.spelling.clone(),
                    },
                    normalized_q,
                    match_mode,
                )?,
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    for (dialect, variant_id, spelling) in [
                        (Dialect::Uk, uk.id, &uk.spelling),
                        (Dialect::Us, us.id, &us.spelling),
                    ] {
                        include_related_v3_match(
                            &mut matches,
                            RelatedWordMatchV3 {
                                pos_id: pos.pos_id,
                                form_id: form.id,
                                variant_id,
                                form_type: form.form_type,
                                dialect,
                                spelling: spelling.clone(),
                            },
                            normalized_q,
                            match_mode,
                        )?;
                    }
                }
            }
        }
    }
    Ok(matches)
}

fn related_v3_sense_gloss(sense: &WordSenseV3) -> String {
    sense
        .definitions
        .iter()
        .find_map(|definition| match definition {
            crate::lexicon::dto::WordDefinitionV3::ZhDefinition { content, .. }
            | crate::lexicon::dto::WordDefinitionV3::ZhSentence { content, .. } => {
                Some(content.text().to_owned())
            }
            crate::lexicon::dto::WordDefinitionV3::EnDefinition { .. }
            | crate::lexicon::dto::WordDefinitionV3::EnSentence { .. } => None,
        })
        .unwrap_or_default()
}

// --- query ---

impl LexiconService {
    pub async fn publication_history(
        &self,
        entry_id: Uuid,
        include_v3: bool,
    ) -> Result<AdminWordPublicationListResponse, LexiconServiceError> {
        let records = self
            .repository
            .publication_history(entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        let publications = records
            .into_iter()
            .map(publication_from_record)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|publication| {
                include_v3 || matches!(publication, AdminWordPublicationAny::V2(_))
            })
            .collect();
        Ok(AdminWordPublicationListResponse { publications })
    }

    pub async fn publication(
        &self,
        entry_id: Uuid,
        publication_id: Uuid,
    ) -> Result<AdminWordPublicationEnvelope, LexiconServiceError> {
        let record = self
            .repository
            .publication(entry_id, publication_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::PublicationNotFound)?;
        Ok(AdminWordPublicationEnvelope {
            publication: publication_from_record(record)?,
        })
    }

    pub async fn get(&self, id: Uuid) -> Result<AdminWordV2Envelope, LexiconServiceError> {
        let record = self
            .repository
            .entry_by_id(id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        let mut word = entry_from_record(record)?;
        self.hydrate_sentence_associations(&mut word).await?;
        Ok(AdminWordV2Envelope { word })
    }

    /// 编辑器打开草稿走这里：除词条本体外还带上已退役的稳定槽位身份。
    ///
    /// 命令类接口只回 [`AdminWordV2Envelope`]，因为提交方自己就知道刚退役了什么；
    /// 刷新和换设备才需要服务端把身份补回来。
    pub async fn get_draft(
        &self,
        id: Uuid,
    ) -> Result<AdminWordDraftV2Envelope, LexiconServiceError> {
        let word = self.get(id).await?.word;
        let retired_stable_slots = self
            .repository
            .retired_stable_slots(id)
            .await
            .map_err(repository_error)?
            .into_iter()
            .map(|record| RetiredStableSlotV2 {
                id: record.id,
                parent_node_id: record.parent_node_id,
                node_role: record.node_role,
            })
            .collect();
        Ok(AdminWordDraftV2Envelope {
            word,
            retired_stable_slots,
        })
    }

    pub async fn related_search(
        &self,
        actor_id: Uuid,
        query: RelatedSearchQuery,
        include_v3: bool,
    ) -> Result<RelatedSearchResponse, LexiconServiceError> {
        if query.page_size.is_some() && query.limit.is_some() {
            return Err(LexiconServiceError::InvalidField {
                field: "page_size",
                message: "page_size and deprecated limit must not be sent together",
            });
        }
        let page_size = query.page_size.or(query.limit).unwrap_or(20);
        let size_field = if query.page_size.is_some() {
            "page_size"
        } else {
            "limit"
        };
        if !(1..=100).contains(&page_size) {
            return Err(LexiconServiceError::InvalidField {
                field: size_field,
                message: "page size must be between 1 and 100",
            });
        }
        let v2 = query.match_mode.is_some()
            || query.exclude_exact.is_some()
            || query.page_size.is_some()
            || query.cursor.is_some();
        let q = query.q.unwrap_or_default();
        if q.contains('\0') {
            return Err(LexiconServiceError::InvalidField {
                field: "q",
                message: "q must not contain NUL characters",
            });
        }
        let q = q.trim();
        if q.is_empty() {
            return Ok(related_search_response(v2, Vec::new(), 0, None));
        }
        let normalized_q = normalize_headword(q)
            .map_err(|_| LexiconServiceError::InvalidField {
                field: "q",
                message: "q is not a valid headword search",
            })?
            .key;
        let match_mode = query.match_mode.unwrap_or(RelatedSearchMatchMode::Contains);
        let exclude_exact = query.exclude_exact.unwrap_or(false);
        let requested_cursor = query.cursor.is_some();
        let mut cursor = if let Some(encoded) = query.cursor.as_deref() {
            let cursor = decode_related_search_cursor(encoded, &self.related_search_cursor_key)
                .map_err(|_| LexiconServiceError::InvalidField {
                    field: "cursor",
                    message: "cursor is invalid",
                })?;
            if cursor.actor_id != actor_id
                || cursor.q != normalized_q
                || cursor.kind != query.kind
                || cursor.match_mode != match_mode
                || cursor.exclude_exact != exclude_exact
                || cursor.include_v3 != include_v3
                || cursor.page_size != page_size
            {
                return Err(LexiconServiceError::InvalidField {
                    field: "cursor",
                    message: "cursor does not match this search",
                });
            }
            cursor
        } else {
            RelatedSearchCursor {
                actor_id,
                q: normalized_q.clone(),
                kind: query.kind,
                match_mode,
                exclude_exact,
                include_v3,
                page_size,
                total: 0,
                consumed: 0,
                last_kind: None,
                last_headword: None,
                last_word_id: None,
                dataset_version: 0,
            }
        };
        let mut attempts = 0;
        let records = loop {
            let dataset_version = self
                .repository
                .related_search_dataset_version()
                .await
                .map_err(repository_error)?;
            if requested_cursor && cursor.dataset_version != dataset_version {
                return Err(LexiconServiceError::InvalidField {
                    field: "cursor",
                    message: "related search targets changed; restart the search",
                });
            }
            if !requested_cursor {
                cursor.dataset_version = dataset_version;
            }
            let records = self
                .repository
                .related_search(&RelatedSearchFilter {
                    q: &normalized_q,
                    kind: query.kind,
                    include_v3,
                    exact: match_mode == RelatedSearchMatchMode::Exact,
                    exclude_exact,
                    limit: i64::from(page_size),
                    last_kind: cursor.last_kind,
                    last_headword: cursor.last_headword.as_deref(),
                    last_word_id: cursor.last_word_id,
                })
                .await
                .map_err(repository_error)?;
            let current_version = self
                .repository
                .related_search_dataset_version()
                .await
                .map_err(repository_error)?;
            if current_version == dataset_version {
                break records;
            }
            if requested_cursor {
                return Err(LexiconServiceError::InvalidField {
                    field: "cursor",
                    message: "related search targets changed; restart the search",
                });
            }
            attempts += 1;
            if attempts == 3 {
                return Err(repository_error(LexiconRepositoryError::Invariant(
                    "related search targets kept changing while opening a cursor",
                )));
            }
        };
        let page_total = records
            .first()
            .map_or(0, |record| record.total.max(0) as u64);
        let total = if cursor.consumed == 0 {
            page_total
        } else {
            cursor.total
        };
        let last_page_key = records.last().map(|record| record.sort_headword.clone());
        let results = records
            .into_iter()
            .map(|record| match record.content_schema_version {
                2 => {
                    let word: AdminWordV2 =
                        serde_json::from_value(record.snapshot).map_err(serialization_error)?;
                    let headword_variants = ordered_headword_sides(&word.headwords)
                        .into_iter()
                        .map(|(dialect, headword)| HeadwordVariant {
                            dialect,
                            headword: headword.to_owned(),
                        })
                        .collect::<Vec<_>>();
                    let dialects = headword_variants
                        .iter()
                        .map(|variant| variant.dialect)
                        .collect();
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
                    Ok(RelatedWordResultAny::V2(RelatedWordResult {
                        schema_version: 2,
                        word_id: word.id,
                        headword: published_word_headword(&word),
                        kind: word.kind,
                        dialects,
                        headword_variants,
                        pos_labels: record.pos_labels,
                        senses,
                    }))
                }
                3 => {
                    let word: AdminWordV3 =
                        serde_json::from_value(record.snapshot).map_err(serialization_error)?;
                    let matches = related_v3_matches(&word, &normalized_q, match_mode)?;
                    if matches.is_empty() {
                        return Err(repository_error(LexiconRepositoryError::Invariant(
                            "related-search V3 surface does not exist in its publication snapshot",
                        )));
                    }
                    let senses = word
                        .meanings
                        .pos
                        .iter()
                        .flat_map(|pos| &pos.senses)
                        .map(|sense| RelatedWordSenseV3 {
                            sense_id: sense.id,
                            gloss: related_v3_sense_gloss(sense),
                        })
                        .collect();
                    Ok(RelatedWordResultAny::V3(RelatedWordResultV3 {
                        schema_version: 3,
                        entry_id: word.id,
                        kind: word.kind,
                        presentation: word.presentation.clone(),
                        matches,
                        senses,
                    }))
                }
                version => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
            })
            .collect::<Result<Vec<_>, LexiconServiceError>>()?;
        let consumed = cursor.consumed + results.len() as u64;
        let next_cursor = (v2 && !results.is_empty() && consumed < total).then(|| {
            let last = results.last().expect("non-empty page has a last result");
            let (last_kind, last_word_id) = match last {
                RelatedWordResultAny::V2(result) => (result.kind, result.word_id),
                RelatedWordResultAny::V3(result) => (EntryKind::Word, result.entry_id),
            };
            let next = RelatedSearchCursor {
                total,
                consumed,
                last_kind: Some(last_kind),
                last_headword: last_page_key,
                last_word_id: Some(last_word_id),
                ..cursor
            };
            encode_related_search_cursor(&next, &self.related_search_cursor_key)
        });
        Ok(related_search_response(v2, results, total, next_cursor))
    }

    pub async fn list(
        &self,
        query: AdminWordListQuery,
        include_v3: bool,
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
            include_v3,
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
            .map(|record| {
                // 方言与拼写按同一排序键成对取回；先配对再解析，
                // 解析失败的方言会连着它那一侧拼写一起丢掉，两个数组不会错位。
                let headword_variants = record
                    .dialects
                    .iter()
                    .zip(record.headword_spellings)
                    .filter_map(|(dialect, headword)| {
                        parse_dialect(dialect).map(|dialect| HeadwordVariant { dialect, headword })
                    })
                    .collect::<Vec<_>>();
                let headword = headword_variants
                    .iter()
                    .map(|variant| variant.headword.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ");
                let status = if record.is_archived {
                    AdminWordStatus::Archived
                } else if record.is_published {
                    AdminWordStatus::Published
                } else {
                    AdminWordStatus::Draft
                };
                match record.content_schema_version {
                    2 => Ok(AdminWordListItem {
                        schema_version: 2,
                        id: record.id,
                        headword,
                        kind: parse_kind(&record.kind).unwrap_or(EntryKind::Word),
                        source_dialect: record
                            .source_dialect
                            .as_deref()
                            .and_then(parse_source_dialect),
                        dialects: headword_variants
                            .iter()
                            .map(|variant| variant.dialect)
                            .collect(),
                        headword_variants,
                        revision: record.revision,
                        lifecycle_revision: record.lifecycle_revision,
                        gloss: record.gloss,
                        pos_list: record.pos_list,
                        levels: record.levels,
                        status,
                        published_revision: record.published_revision,
                        has_unpublished_changes: record.has_unpublished_changes,
                        max_reachable_step: max_reachable_step(&record.completed_steps),
                        created_by_name: record.created_by_name,
                        created_at: record.created_at,
                        updated_at: record.updated_at,
                    }
                    .into()),
                    3 => {
                        let presentation = match (
                            record.presentation_label,
                            record.presentation_surfaces,
                            record.presentation_strategy,
                        ) {
                            (Some(label), Some(matched_surfaces), Some(strategy_version)) => {
                                EntryPresentationV3 {
                                    label,
                                    matched_surfaces,
                                    strategy_version,
                                }
                            }
                            _ if !headword_variants.is_empty() => {
                                let bridge = legacy_bridge_from_list(
                                    &headword_variants,
                                    record.source_dialect.as_deref(),
                                )?;
                                crate::lexicon::v3_projection::presentation_from_legacy_bridge(
                                    record.id, &bridge,
                                )
                            }
                            _ => {
                                let forms: DraftFormsStepContentV3 =
                                    serde_json::from_value(record.forms)
                                        .map_err(serialization_error)?;
                                crate::lexicon::v3_projection::presentation_from_native_forms(
                                    record.id, &forms,
                                )
                                .map_err(|_| invariant_record())?
                            }
                        };
                        Ok(AdminWordListItemAny::V3(AdminWordListItemV3 {
                            schema_version: 3,
                            id: record.id,
                            kind: WordEntryKindV3::Word,
                            presentation,
                            revision: record.revision,
                            lifecycle_revision: record.lifecycle_revision,
                            gloss: record.gloss,
                            pos_list: record.pos_list,
                            levels: record.levels,
                            status,
                            published_revision: record.published_revision,
                            has_unpublished_changes: record.has_unpublished_changes,
                            max_reachable_step: max_reachable_step(&record.completed_steps),
                            created_by_name: record.created_by_name,
                            created_at: record.created_at,
                            updated_at: record.updated_at,
                        }))
                    }
                    version => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
                }
            })
            .collect::<Result<Vec<_>, LexiconServiceError>>()?;
        Ok(AdminWordListResponse {
            words,
            page: AdminWordListPage {
                page,
                page_size,
                total,
            },
        })
    }

    pub async fn stats(&self, include_v3: bool) -> Result<AdminWordStats, LexiconServiceError> {
        let record = self
            .repository
            .stats(include_v3)
            .await
            .map_err(repository_error)?;
        Ok(AdminWordStats {
            total: record.total,
            today: record.today,
            month: record.month,
        })
    }
}

fn publication_from_record(
    record: PublicationReadRecord,
) -> Result<AdminWordPublicationAny, LexiconServiceError> {
    match record.content_schema_version {
        2 => {
            let word: AdminWordV2 =
                serde_json::from_value(record.snapshot).map_err(serialization_error)?;
            Ok(AdminWordPublicationAny::V2(Box::new(
                AdminWordPublicationV2 {
                    schema_version: 2,
                    publication_id: record.id,
                    entry_id: record.entry_id,
                    publication_number: record.publication_number,
                    source_revision: record.source_revision,
                    word,
                    published_by_admin_id: record.published_by_admin_id,
                    published_at: record.published_at,
                    is_current: record.is_current,
                },
            )))
        }
        3 => {
            let word: AdminWordV3 =
                serde_json::from_value(record.snapshot).map_err(serialization_error)?;
            Ok(AdminWordPublicationAny::V3(Box::new(
                AdminWordPublicationV3 {
                    schema_version: 3,
                    publication_id: record.id,
                    entry_id: record.entry_id,
                    publication_number: record.publication_number,
                    source_revision: record.source_revision,
                    word,
                    published_by_admin_id: record.published_by_admin_id,
                    published_at: record.published_at,
                    is_current: record.is_current,
                },
            )))
        }
        version => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
    }
}

fn legacy_bridge_from_list(
    variants: &[HeadwordVariant],
    source_dialect: Option<&str>,
) -> Result<LegacyHeadwordsCompatibilityV3, LexiconServiceError> {
    if let Some(common) = variants
        .iter()
        .find(|variant| variant.dialect == Dialect::Common)
    {
        return Ok(LegacyHeadwordsCompatibilityV3::Unified {
            common: common.headword.clone(),
        });
    }
    let uk = variants
        .iter()
        .find(|variant| variant.dialect == Dialect::Uk)
        .ok_or_else(invariant_record)?;
    let us = variants
        .iter()
        .find(|variant| variant.dialect == Dialect::Us)
        .ok_or_else(invariant_record)?;
    Ok(LegacyHeadwordsCompatibilityV3::Distinguish {
        uk: uk.headword.clone(),
        us: us.headword.clone(),
        source_dialect: match source_dialect {
            Some("uk") => SourceDialect::Uk,
            Some("us") => SourceDialect::Us,
            _ => return Err(invariant_record()),
        },
    })
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

#[cfg(test)]
mod related_search_cursor_tests {
    use super::*;

    fn cursor() -> RelatedSearchCursor {
        RelatedSearchCursor {
            actor_id: Uuid::nil(),
            q: "workspace".to_owned(),
            kind: Some(EntryKind::Word),
            match_mode: RelatedSearchMatchMode::Exact,
            exclude_exact: false,
            include_v3: true,
            page_size: 20,
            total: 40,
            consumed: 20,
            last_kind: Some(EntryKind::Word),
            last_headword: Some("workspace".to_owned()),
            last_word_id: Some(Uuid::nil()),
            dataset_version: 7,
        }
    }

    #[test]
    fn cursor_round_trip_is_keyed_and_tamper_evident() {
        let encoded = encode_related_search_cursor(&cursor(), b"correct-key");
        let decoded = decode_related_search_cursor(&encoded, b"correct-key").unwrap();
        assert_eq!(decoded.actor_id, Uuid::nil());
        assert_eq!(decoded.consumed, 20);
        assert!(decoded.include_v3);
        assert_eq!(decoded.dataset_version, 7);
        assert!(decode_related_search_cursor(&encoded, b"wrong-key").is_err());

        let mut bytes = URL_SAFE_NO_PAD.decode(&encoded).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        let tampered = URL_SAFE_NO_PAD.encode(bytes);
        assert!(decode_related_search_cursor(&tampered, b"correct-key").is_err());
    }
}
