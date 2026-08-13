use super::*;

// --- aggregate ---

pub(super) fn entry_from_record(record: EntryRecord) -> Result<AdminWordV2, LexiconServiceError> {
    let headwords = match record.headword_mode.as_str() {
        "unified" => WordHeadwordsV2::Unified {
            common: record.common_headword.ok_or_else(invariant_record)?,
        },
        "distinguish" => WordHeadwordsV2::Distinguish {
            uk: record.uk_headword.ok_or_else(invariant_record)?,
            us: record.us_headword.ok_or_else(invariant_record)?,
            source_dialect: match record.source_dialect.as_deref() {
                Some("uk") => SourceDialect::Uk,
                Some("us") => SourceDialect::Us,
                _ => return Err(invariant_record()),
            },
        },
        _ => return Err(invariant_record()),
    };
    let detection_snapshot =
        serde_json::from_value(record.detection_snapshot).map_err(|error| {
            LexiconServiceError::Repository(LexiconRepositoryError::Serialization(error))
        })?;
    let forms = serde_json::from_value(record.forms).map_err(|error| {
        LexiconServiceError::Repository(LexiconRepositoryError::Serialization(error))
    })?;
    let meanings = serde_json::from_value(record.meanings).map_err(|error| {
        LexiconServiceError::Repository(LexiconRepositoryError::Serialization(error))
    })?;
    let completed_steps = record
        .completed_steps
        .iter()
        .filter_map(|step| match step.as_str() {
            "basics" => Some(PersistedWordStep::Basics),
            "forms" => Some(PersistedWordStep::Forms),
            "meanings" => Some(PersistedWordStep::Meanings),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(AdminWordV2 {
        schema_version: record.content_schema_version as u8,
        id: record.id,
        language: record.language,
        kind: parse_kind(&record.kind).ok_or_else(invariant_record)?,
        status: if record.archived_at.is_some() {
            AdminWordStatus::Archived
        } else if record.current_publication_id.is_some() {
            AdminWordStatus::Published
        } else {
            AdminWordStatus::Draft
        },
        revision: record.revision,
        lifecycle_revision: record.lifecycle_revision,
        published_revision: record.current_publication_source_revision,
        has_unpublished_changes: record
            .current_publication_source_revision
            .is_some_and(|revision| revision != record.revision),
        headwords,
        frequency: record.frequency,
        detection_snapshot,
        forms,
        meanings,
        max_reachable_step: max_reachable_step(&record.completed_steps),
        completed_steps,
        created_by: record.created_by_admin_id,
        created_at: record.created_at,
        updated_at: record.updated_at,
        archived_at: record.archived_at,
        archived_by: record.archived_by_admin_id,
        published_at: record.current_published_at,
    })
}

pub(super) fn build_suggested_forms(
    headwords: &WordHeadwordsV2,
    catalog_parts: &[CatalogPartRecord],
) -> DraftFormsStepContent {
    let distinguish = matches!(headwords, WordHeadwordsV2::Distinguish { .. });
    DraftFormsStepContent {
        pos: catalog_parts
            .iter()
            .map(|part| {
                let variants = match headwords {
                    WordHeadwordsV2::Unified { common } => {
                        vec![base_variant(
                            Dialect::Common,
                            common,
                            TextOrigin::Dictionary,
                        )]
                    }
                    WordHeadwordsV2::Distinguish { uk, us, .. } => {
                        vec![
                            base_variant(Dialect::Uk, uk, TextOrigin::Dictionary),
                            base_variant(Dialect::Us, us, TextOrigin::Dictionary),
                        ]
                    }
                };
                WordPosFormsV2 {
                    pos_id: Uuid::now_v7(),
                    pos: part.code.clone(),
                    dialect_rules: DialectRulesV2 {
                        spelling_mode: if distinguish {
                            "distinguish"
                        } else {
                            "unified"
                        }
                        .to_owned(),
                        phonetic_mode: if distinguish {
                            "distinguish"
                        } else {
                            "unified"
                        }
                        .to_owned(),
                    },
                    base_form: WordBaseFormSlotV2 {
                        id: Uuid::now_v7(),
                        form_type: "base".to_owned(),
                        variants,
                    },
                    form_groups: vec![WordFormGroupV2 {
                        id: Uuid::now_v7(),
                        is_regular: true,
                        slots: Vec::new(),
                    }],
                }
            })
            .collect(),
    }
}

pub(super) fn base_variant(
    dialect: Dialect,
    spelling: &str,
    origin: TextOrigin,
) -> WordFormVariantV2 {
    WordFormVariantV2 {
        id: Uuid::now_v7(),
        dialect,
        spelling: spelling.to_owned(),
        origin,
        pronunciations: vec![WordPronunciationV2 {
            id: Uuid::now_v7(),
            dict_phonetic: String::new(),
            actual_pron: String::new(),
            style: PronunciationStyle::Normal,
        }],
    }
}

pub(super) fn build_initial_meanings(
    word_id: Uuid,
    headwords: &WordHeadwordsV2,
    forms: &DraftFormsStepContent,
) -> DraftMeaningsStepContent {
    let sense_group_id = Uuid::now_v7();
    DraftMeaningsStepContent {
        sense_groups: vec![SenseGroupV2 {
            id: sense_group_id,
            name_zh: String::new(),
            name_en: String::new(),
        }],
        pos: forms
            .pos
            .iter()
            .map(|forms_pos| {
                build_initial_pos_meanings(word_id, headwords, forms_pos, sense_group_id)
            })
            .collect(),
    }
}

pub(super) fn build_initial_pos_meanings(
    word_id: Uuid,
    headwords: &WordHeadwordsV2,
    forms_pos: &WordPosFormsV2,
    sense_group_id: Uuid,
) -> WordPosMeaningsV2 {
    let grammar_variants = match headwords {
        WordHeadwordsV2::Unified { .. } => vec![GrammarVariantV2 {
            id: Uuid::now_v7(),
            dialect: Dialect::Common,
            content: RichText::empty(),
        }],
        WordHeadwordsV2::Distinguish { .. } => vec![
            GrammarVariantV2 {
                id: Uuid::now_v7(),
                dialect: Dialect::Uk,
                content: RichText::empty(),
            },
            GrammarVariantV2 {
                id: Uuid::now_v7(),
                dialect: Dialect::Us,
                content: RichText::empty(),
            },
        ],
    };
    let sense_id = Uuid::now_v7();
    WordPosMeaningsV2 {
        pos_id: forms_pos.pos_id,
        grammar_structures: vec![GrammarStructureV2 {
            id: Uuid::now_v7(),
            variants: grammar_variants,
        }],
        senses: vec![WordSenseV2 {
            id: sense_id,
            sub_pos: String::new(),
            level: "A1".to_owned(),
            sense_group_id: Some(sense_group_id),
            frequency: None,
            depends_on_context: false,
            definitions: vec![WordDefinitionV2::ZhDefinition {
                id: Uuid::now_v7(),
                content_id: Uuid::now_v7(),
                level: "A1".to_owned(),
                grammar_structure_id: None,
                content: RichText::empty(),
            }],
            sentences: vec![WordSentenceV2 {
                id: Uuid::now_v7(),
                level: "A1".to_owned(),
                en_text: empty_english_text(headwords),
                zh_text_id: Uuid::now_v7(),
                zh_text: RichText::empty(),
                links: vec![WordSentenceLinkV2 {
                    word_id,
                    sense_id,
                    role: "focus".to_owned(),
                }],
            }],
            relations: Vec::new(),
        }],
    }
}

pub(super) fn empty_english_text(headwords: &WordHeadwordsV2) -> EnglishTextV2 {
    match headwords {
        WordHeadwordsV2::Unified { .. } => EnglishTextV2::Unified {
            common: TextVariantV2 {
                id: Uuid::now_v7(),
                value: RichText::empty(),
                origin: TextOrigin::Manual,
            },
        },
        WordHeadwordsV2::Distinguish { source_dialect, .. } => {
            let ready = || DialectVariantSlotV2::Ready {
                variant: TextVariantV2 {
                    id: Uuid::now_v7(),
                    value: RichText::empty(),
                    origin: TextOrigin::Manual,
                },
            };
            EnglishTextV2::Distinguish {
                source_dialect: *source_dialect,
                uk: if *source_dialect == SourceDialect::Uk {
                    ready()
                } else {
                    DialectVariantSlotV2::Missing
                },
                us: if *source_dialect == SourceDialect::Us {
                    ready()
                } else {
                    DialectVariantSlotV2::Missing
                },
            }
        }
    }
}

pub(super) fn align_base_forms(
    forms: &mut DraftFormsStepContent,
    detected: &WordHeadwordsV2,
    matched_dialect: Dialect,
    submitted: &WordHeadwordsV2,
) {
    let mode = if matches!(submitted, WordHeadwordsV2::Distinguish { .. }) {
        "distinguish"
    } else {
        "unified"
    };
    for pos in &mut forms.pos {
        pos.dialect_rules.spelling_mode = mode.to_owned();
        pos.dialect_rules.phonetic_mode = mode.to_owned();
        pos.base_form.variants = match submitted {
            WordHeadwordsV2::Unified { common } => vec![base_variant(
                Dialect::Common,
                common,
                headword_origin(
                    detected,
                    matched_dialect,
                    submitted,
                    Dialect::Common,
                    common,
                ),
            )],
            WordHeadwordsV2::Distinguish { uk, us, .. } => {
                vec![
                    base_variant(
                        Dialect::Uk,
                        uk,
                        headword_origin(detected, matched_dialect, submitted, Dialect::Uk, uk),
                    ),
                    base_variant(
                        Dialect::Us,
                        us,
                        headword_origin(detected, matched_dialect, submitted, Dialect::Us, us),
                    ),
                ]
            }
        };
    }
}

// --- creation ---

impl LexiconService {
    pub async fn detect(
        &self,
        actor_id: Uuid,
        input: DetectWordInputV2,
    ) -> Result<DetectWordResponseV2, LexiconServiceError> {
        if input.language != "en" {
            return Err(LexiconServiceError::UnsupportedLanguage);
        }
        let normalized = normalize_headword(&input.headword).map_err(map_headword_error)?;
        let request = DetectionRequestEcho {
            language: "en".to_owned(),
            headword: normalized.display.clone(),
        };
        let term = self
            .repository
            .dictionary_term(&normalized.key)
            .await
            .map_err(repository_error)?;

        let (entry_kind, matched_dialect, builtin_dictionary, duplicate_keys) =
            if let Some(term) = term {
                let entry_kind = parse_kind(&term.kind).unwrap_or_else(|| {
                    if normalized.display.contains(' ') {
                        EntryKind::Phrase
                    } else {
                        EntryKind::Word
                    }
                });
                let surface = self
                    .repository
                    .region_surface(&normalized.key)
                    .await
                    .map_err(repository_error)?;
                let (headwords, matched_dialect) = self
                    .detected_headwords(&term.term, &term.region_family, surface)
                    .await?;
                let normalized_keys = normalized_headword_keys(&headwords)?;
                let mapped_codes = map_dictionary_pos(&term.pos);
                let catalog_parts = self
                    .repository
                    .catalog_parts(&mapped_codes)
                    .await
                    .map_err(repository_error)?;
                let suggested_forms = build_suggested_forms(&headwords, &catalog_parts);
                let provider = DictionaryProviderV2 {
                    name: term.provider_name,
                    version: term.provider_version,
                };
                (
                    entry_kind,
                    Some(matched_dialect),
                    BuiltinDictionaryResultV2::Matched {
                        provider: provider.clone(),
                        headwords,
                        suggested_forms: Box::new(suggested_forms),
                        suggested_meanings: Box::new(DraftMeaningsStepContent::default()),
                        suggested_frequency: None,
                        coverage: DictionaryCoverageV2 {
                            forms: DictionaryCoverageStateV2::Partial,
                            pronunciations: DictionaryCoverageStateV2::Missing,
                            meanings: DictionaryCoverageStateV2::Missing,
                            examples: DictionaryCoverageStateV2::Missing,
                            frequency: DictionaryCoverageStateV2::Missing,
                        },
                        provenance: DictionaryProvenanceV2 {
                            forms: Some(provider),
                            pronunciations: None,
                            meanings: None,
                            examples: None,
                            frequency: None,
                        },
                    },
                    normalized_keys,
                )
            } else {
                let entry_kind = if normalized.display.contains(' ') {
                    EntryKind::Phrase
                } else {
                    EntryKind::Word
                };
                (
                    entry_kind,
                    None,
                    BuiltinDictionaryResultV2::NotFound,
                    vec![normalized.key.clone()],
                )
            };

        let duplicates = self
            .repository
            .duplicates(entry_kind, &duplicate_keys)
            .await
            .map_err(repository_error)?
            .into_iter()
            .map(|record| DuplicateWordMatchV2 {
                word_id: record.entry_id,
                headword: record.headword,
                dialect: parse_dialect(&record.dialect).unwrap_or(Dialect::Common),
                status: if record.is_archived {
                    AdminWordStatus::Archived
                } else if record.is_published {
                    AdminWordStatus::Published
                } else {
                    AdminWordStatus::Draft
                },
            })
            .collect::<Vec<_>>();
        let smart_dictionary = if duplicates.is_empty() {
            SmartDictionaryResultV2::Clear { duplicates }
        } else {
            SmartDictionaryResultV2::Duplicate { duplicates }
        };

        let detection = DetectWordResponseV2 {
            detection_id: Uuid::now_v7(),
            expires_at: Utc::now()
                + Duration::from_std(DETECTION_TTL).expect("five minutes is valid"),
            request,
            normalized_headword: normalized.key,
            entry_kind,
            matched_dialect,
            builtin_dictionary,
            smart_dictionary,
        };
        self.detections
            .save(actor_id, &detection, DETECTION_RETENTION_TTL)
            .await
            .map_err(LexiconServiceError::DetectionStore)?;
        Ok(detection)
    }

    pub async fn create(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        idempotency_key: Uuid,
        mut input: CreateAdminWordV2Input,
    ) -> Result<AdminWordV2Envelope, LexiconServiceError> {
        if input.schema_version != 2 {
            return Err(LexiconServiceError::InvalidField {
                field: "schema_version",
                message: "schema_version must be 2",
            });
        }
        normalize_submitted_headwords(&mut input.headwords)?;
        let request_hash = sha256_json(&input).map_err(|error| {
            LexiconServiceError::Repository(LexiconRepositoryError::Serialization(error))
        })?;

        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{CREATE_SCOPE}:{actor_id}:{}", idempotency_key))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;

        if let Some(existing) = LexiconRepository::idempotency(
            &mut transaction,
            CREATE_SCOPE,
            actor_id,
            idempotency_key,
        )
        .await
        .map_err(repository_error)?
        {
            if existing.request_hash != request_hash {
                return Err(LexiconServiceError::IdempotencyConflict);
            }
            existing.resource_id.ok_or_else(|| {
                LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
                    "create idempotency record has no resource",
                ))
            })?;
            transaction.commit().await.map_err(database_error)?;
            return serde_json::from_value(existing.response_body).map_err(|error| {
                LexiconServiceError::Repository(LexiconRepositoryError::Serialization(error))
            });
        }

        let detection = self
            .detections
            .load(actor_id, input.detection_id)
            .await
            .map_err(LexiconServiceError::DetectionStore)?
            .ok_or(LexiconServiceError::DetectionMismatch)?;
        if detection.expires_at <= Utc::now() {
            return Err(LexiconServiceError::DetectionExpired);
        }
        let (
            detected_headwords,
            mut forms,
            suggested_meanings,
            suggested_frequency,
            dictionary_provider,
            dictionary_coverage,
            dictionary_provenance,
            matched_dialect,
            builtin_status,
        ) = match (
            &detection.builtin_dictionary,
            &detection.smart_dictionary,
            detection.entry_kind,
            detection.matched_dialect,
        ) {
            (
                BuiltinDictionaryResultV2::Matched {
                    headwords,
                    suggested_forms,
                    suggested_meanings,
                    suggested_frequency,
                    provider,
                    coverage,
                    provenance,
                },
                SmartDictionaryResultV2::Clear { .. },
                EntryKind::Word | EntryKind::Phrase,
                Some(matched_dialect),
            ) => (
                headwords.clone(),
                (**suggested_forms).clone(),
                (**suggested_meanings).clone(),
                suggested_frequency.clone(),
                Some(provider.clone()),
                Some(coverage.clone()),
                Some(provenance.clone()),
                matched_dialect,
                "matched",
            ),
            (
                BuiltinDictionaryResultV2::NotFound,
                SmartDictionaryResultV2::Clear { .. },
                EntryKind::Phrase,
                None,
            ) => {
                let WordHeadwordsV2::Unified { common } = &input.headwords else {
                    return Err(LexiconServiceError::DetectionMismatch);
                };
                if normalize_headword(common).map_err(map_headword_error)?.key
                    != detection.normalized_headword
                {
                    return Err(LexiconServiceError::DetectionMismatch);
                }
                (
                    input.headwords.clone(),
                    DraftFormsStepContent::default(),
                    DraftMeaningsStepContent::default(),
                    None,
                    None,
                    None,
                    None,
                    Dialect::Common,
                    "not_found",
                )
            }
            (_, SmartDictionaryResultV2::Duplicate { .. }, _, _) => {
                return Err(LexiconServiceError::DuplicateWord);
            }
            _ => return Err(LexiconServiceError::DetectionMismatch),
        };
        if !compatible_headwords(&detected_headwords, matched_dialect, &input.headwords)? {
            return Err(LexiconServiceError::DetectionMismatch);
        }
        align_base_forms(
            &mut forms,
            &detected_headwords,
            matched_dialect,
            &input.headwords,
        );
        let pos_codes = forms
            .pos
            .iter()
            .map(|part| part.pos.clone())
            .collect::<Vec<_>>();
        let parts = LexiconRepository::catalog_parts_for_reference(&mut transaction, &pos_codes)
            .await
            .map_err(repository_error)?;
        let part_map = parts
            .into_iter()
            .map(|part| (part.code, part.id))
            .collect::<HashMap<_, _>>();
        if part_map.len() != pos_codes.len() {
            return Err(LexiconServiceError::CatalogMismatch);
        }

        let word_id = Uuid::now_v7();
        let now = Utc::now();
        let meanings = if suggested_meanings.pos.is_empty() {
            build_initial_meanings(word_id, &input.headwords, &forms)
        } else {
            suggested_meanings
        };
        let suggested_pos = forms.pos.iter().map(|part| part.pos.clone()).collect();
        let detection_snapshot = crate::lexicon::dto::WordDetectionSnapshotV2 {
            detection_id: detection.detection_id,
            request: detection.request.clone(),
            normalized_headword: detection.normalized_headword.clone(),
            entry_kind: detection.entry_kind,
            matched_dialect,
            builtin_dictionary_status: builtin_status.to_owned(),
            smart_dictionary_status: "clear".to_owned(),
            headwords: detected_headwords,
            suggested_pos,
            dictionary_provider,
            dictionary_coverage,
            dictionary_provenance,
            detected_at: now,
        };
        let word = AdminWordV2 {
            schema_version: 2,
            id: word_id,
            language: "en".to_owned(),
            kind: detection.entry_kind,
            status: AdminWordStatus::Draft,
            revision: 1,
            lifecycle_revision: 1,
            published_revision: None,
            has_unpublished_changes: false,
            headwords: input.headwords,
            frequency: suggested_frequency,
            detection_snapshot,
            forms,
            meanings,
            completed_steps: vec![PersistedWordStep::Basics],
            max_reachable_step: WordCreationStep::Forms,
            created_by: actor_id,
            created_at: now,
            updated_at: now,
            archived_at: None,
            archived_by: None,
            published_at: None,
        };
        if !LexiconRepository::consume_detection(
            &mut transaction,
            actor_id,
            input.detection_id,
            word_id,
        )
        .await
        .map_err(repository_error)?
        {
            return Err(LexiconServiceError::DetectionMismatch);
        }
        LexiconRepository::insert_entry(
            &mut transaction,
            &word,
            actor_id,
            request_id,
            &part_map,
            idempotency_key,
            &request_hash,
        )
        .await
        .map_err(repository_error)?;
        transaction.commit().await.map_err(database_error)?;

        if let Err(error) = self.detections.remove(actor_id, input.detection_id).await {
            tracing::warn!(%error, detection_id = %input.detection_id, "created lexicon entry but failed to remove detection context");
        }
        Ok(AdminWordV2Envelope { word })
    }
}

// --- support ---

impl LexiconService {
    pub(super) async fn detected_headwords(
        &self,
        term: &str,
        term_family: &str,
        surface: Option<RegionSurfaceRecord>,
    ) -> Result<(WordHeadwordsV2, Dialect), LexiconServiceError> {
        let Some(surface) = surface else {
            return Ok((
                WordHeadwordsV2::Unified {
                    common: term.to_owned(),
                },
                family_dialect(term_family).unwrap_or(Dialect::Common),
            ));
        };
        let effective_family = surface.region_family.as_str();
        let target_keys = surface
            .targets
            .iter()
            .filter_map(|target| normalize_headword(target).ok().map(|value| value.key))
            .collect::<Vec<_>>();
        let mut candidates = self
            .repository
            .dictionary_candidates(&target_keys)
            .await
            .map_err(repository_error)?;
        let source_dialect = family_dialect(effective_family).or_else(|| {
            let mut target_dialects = candidates
                .iter()
                .filter_map(|candidate| family_dialect(&candidate.region_family));
            let first = target_dialects.next()?;
            target_dialects
                .all(|dialect| dialect == first)
                .then_some(match first {
                    Dialect::Uk => Dialect::Us,
                    Dialect::Us => Dialect::Uk,
                    Dialect::Common => unreachable!("family dialect is never common"),
                })
        });
        let Some(source_dialect) = source_dialect else {
            return Ok((
                WordHeadwordsV2::Unified {
                    common: surface.term,
                },
                Dialect::Common,
            ));
        };
        let priority = |candidate: &DictionaryCandidateRecord| match family_dialect(
            &candidate.region_family,
        ) {
            Some(dialect) if dialect != source_dialect => 0_u8,
            None => 1,
            Some(_) => 2,
        };
        candidates.sort_by(|left, right| {
            priority(left)
                .cmp(&priority(right))
                .then_with(|| {
                    target_keys
                        .iter()
                        .position(|key| key == &left.normalized_term)
                        .unwrap_or(usize::MAX)
                        .cmp(
                            &target_keys
                                .iter()
                                .position(|key| key == &right.normalized_term)
                                .unwrap_or(usize::MAX),
                        )
                })
                .then_with(|| left.normalized_term.cmp(&right.normalized_term))
        });
        let counterpart = candidates
            .into_iter()
            .find(|candidate| priority(candidate) < 2);
        let Some(counterpart) = counterpart else {
            return Ok((
                WordHeadwordsV2::Unified {
                    common: surface.term,
                },
                source_dialect,
            ));
        };
        let (uk, us, source_dialect_value) = match source_dialect {
            Dialect::Uk => (surface.term, counterpart.term, SourceDialect::Uk),
            Dialect::Us => (counterpart.term, surface.term, SourceDialect::Us),
            Dialect::Common => unreachable!("family dialect never returns common"),
        };
        Ok((
            WordHeadwordsV2::Distinguish {
                uk,
                us,
                source_dialect: source_dialect_value,
            },
            source_dialect,
        ))
    }

    pub(super) async fn catalog_context(
        &self,
        forms: &DraftFormsStepContent,
    ) -> Result<CatalogContext, LexiconServiceError> {
        let codes = forms
            .pos
            .iter()
            .map(|part| part.pos.clone())
            .collect::<Vec<_>>();
        let parts = self
            .repository
            .catalog_parts(&codes)
            .await
            .map_err(repository_error)?;
        let sub_parts = self
            .repository
            .catalog_sub_parts()
            .await
            .map_err(repository_error)?;
        Ok(CatalogContext {
            part_codes: parts.iter().map(|part| part.code.clone()).collect(),
            part_ids: parts.into_iter().map(|part| (part.code, part.id)).collect(),
            sub_part_ids: sub_parts
                .iter()
                .map(|part| (part.code.clone(), part.id))
                .collect(),
            sub_part_parents: sub_parts
                .into_iter()
                .map(|part| (part.code, part.part_code))
                .collect(),
        })
    }

    pub(super) async fn catalog_context_for_reference(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        forms: &DraftFormsStepContent,
    ) -> Result<CatalogContext, LexiconServiceError> {
        let codes = forms
            .pos
            .iter()
            .map(|part| part.pos.clone())
            .collect::<Vec<_>>();
        let parts = LexiconRepository::catalog_parts_for_reference(transaction, &codes)
            .await
            .map_err(repository_error)?;
        let sub_parts = LexiconRepository::catalog_sub_parts_for_reference(transaction)
            .await
            .map_err(repository_error)?;
        Ok(CatalogContext {
            part_codes: parts.iter().map(|part| part.code.clone()).collect(),
            part_ids: parts.into_iter().map(|part| (part.code, part.id)).collect(),
            sub_part_ids: sub_parts
                .iter()
                .map(|part| (part.code.clone(), part.id))
                .collect(),
            sub_part_parents: sub_parts
                .into_iter()
                .map(|part| (part.code, part.part_code))
                .collect(),
        })
    }
}
