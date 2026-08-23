use super::*;

// --- aggregate ---

pub(crate) fn entry_from_record(record: EntryRecord) -> Result<AdminWordV2, LexiconServiceError> {
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
                    form_groups: if crate::lexicon::form_types::allowed_form_types(&part.code)
                        .is_empty()
                    {
                        Vec::new()
                    } else {
                        vec![WordFormGroupV2 {
                            id: Uuid::now_v7(),
                            is_regular: true,
                            slots: Vec::new(),
                        }]
                    },
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
        let normalized = NormalizedHeadword::parse(&input.headword).map_err(map_headword_error)?;
        let request = DetectionRequestEcho {
            language: "en".to_owned(),
            headword: normalized.display.clone(),
        };
        let term = self
            .repository
            .dictionary_term(&normalized.key)
            .await
            .map_err(repository_error)?;

        let (entry_kind, matched_dialect, builtin_dictionary, candidate_headwords) =
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
                        headwords: headwords.clone(),
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
                    headwords,
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
                    WordHeadwordsV2::Unified {
                        common: normalized.display.clone(),
                    },
                )
            };

        let detection_id = Uuid::now_v7();
        let expires_at =
            Utc::now() + Duration::from_std(DETECTION_TTL).expect("five minutes is valid");
        let evidence = CreateDetectionEvidence {
            detection_id,
            expires_at,
            request: request.clone(),
            normalized_headword: normalized.key.clone(),
            entry_kind,
            matched_dialect,
            builtin_dictionary: builtin_dictionary.clone(),
            candidate_headwords: candidate_headwords.clone(),
        };
        let (matches, contexts) = self
            .headword_surface_matches(&candidate_headwords, entry_kind, None)
            .await?;
        let legacy_keys = normalized_headword_keys(&candidate_headwords)?;
        let legacy = self
            .repository
            .legacy_exact_duplicates(entry_kind, &legacy_keys)
            .await
            .map_err(repository_error)?;
        let has_unprojected_legacy_exact = has_unprojected_legacy_exact(&legacy, &matches);
        let smart_dictionary = if has_unprojected_legacy_exact {
            SmartDictionaryResultV2::Duplicate {
                duplicates: legacy
                    .into_iter()
                    .map(|record| DuplicateWordMatchV2 {
                        word_id: record.entry_id,
                        headword: record.headword,
                        dialect: parse_dialect(&record.dialect).unwrap_or(Dialect::Common),
                        // 这条分支只在 legacy exact-headword 索引命中、而投影表还没
                        // 追平时触发（legacy_exact_duplicates 按 kind 精确匹配主词），
                        // 所以命中原因恒为 exact_headword。
                        match_category: SurfaceMatchCategoryV2::ExactHeadword,
                        status: if record.is_archived {
                            AdminWordStatus::Archived
                        } else if record.is_published {
                            AdminWordStatus::Published
                        } else {
                            AdminWordStatus::Draft
                        },
                    })
                    .collect(),
            }
        } else if matches.is_empty() {
            SmartDictionaryResultV2::Clear {
                duplicates: Vec::new(),
            }
        } else {
            let (_, snapshot) = self
                .create_detection_surface_snapshot(actor_id, &evidence, matches, contexts, None)
                .await?;
            SmartDictionaryResultV2::Warning {
                duplicates: Vec::new(),
                surface_match_page: Box::new(snapshot.page),
                matched_entry_contexts: Vec::new(),
            }
        };

        let detection = DetectWordResponseV2 {
            detection_id,
            expires_at,
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
            if existing.response_status >= 400 {
                let failure: CreateIdempotentFailure =
                    serde_json::from_value(existing.response_body).map_err(serialization_error)?;
                transaction.commit().await.map_err(database_error)?;
                return Err(failure.into_service_error());
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

        // The durable consumption row is authoritative once a create command has
        // committed. Same-key retries have already replayed above; every other
        // reuse of the detection has stable gone/consumed semantics.
        if LexiconRepository::consumed_detection(&mut transaction, actor_id, input.detection_id)
            .await
            .map_err(repository_error)?
            .is_some()
        {
            return persist_create_failure(
                transaction,
                actor_id,
                idempotency_key,
                &request_hash,
                CreateIdempotentFailure::DetectionExpired,
            )
            .await;
        }

        let (mut evidence, recovered_confirmation) =
            if let Some(token) = input.confirmed_surface_match_token.as_deref() {
                // A terminal snapshot owner bundle deliberately outlives the short
                // detection-store payload. Recover it only after the token, actor,
                // command, owner context, TTL and policy epoch have been verified.
                let confirmation = match self
                    .surface_snapshots
                    .verify_owner(
                        token,
                        &ExpectedSurfaceOwner {
                            actor_id,
                            command: SurfaceConsumptionCommand::CreateEntry,
                            owner_context: input.detection_id.to_string(),
                        },
                    )
                    .await
                {
                    Ok(confirmation) => confirmation,
                    Err(SurfaceSnapshotError::Expired) => {
                        return persist_create_failure(
                            transaction,
                            actor_id,
                            idempotency_key,
                            &request_hash,
                            CreateIdempotentFailure::SurfaceMatchSnapshotExpired,
                        )
                        .await;
                    }
                    Err(SurfaceSnapshotError::PolicyChanged(policy_name)) => {
                        let policy = self
                            .surface_policies
                            .policy(policy_name)
                            .await
                            .map_err(LexiconServiceError::SurfacePolicy)?;
                        return persist_create_failure(
                            transaction,
                            actor_id,
                            idempotency_key,
                            &request_hash,
                            CreateIdempotentFailure::SurfacePolicyChanged { policy },
                        )
                        .await;
                    }
                    Err(SurfaceSnapshotError::BindingMismatch) => {
                        return Err(LexiconServiceError::DetectionMismatch);
                    }
                    Err(error) => return Err(LexiconServiceError::SurfaceSnapshot(error)),
                };
                let evidence: CreateDetectionEvidence =
                    serde_json::from_value(confirmation.owner_bundle.clone())
                        .map_err(serialization_error)?;
                if evidence.detection_id != input.detection_id {
                    return Err(LexiconServiceError::DetectionMismatch);
                }
                (evidence, Some(confirmation))
            } else {
                let detection = self
                    .detections
                    .load(actor_id, input.detection_id)
                    .await
                    .map_err(LexiconServiceError::DetectionStore)?
                    .ok_or(LexiconServiceError::DetectionMismatch)?;
                if detection.expires_at <= Utc::now() {
                    return persist_create_failure(
                        transaction,
                        actor_id,
                        idempotency_key,
                        &request_hash,
                        CreateIdempotentFailure::DetectionExpired,
                    )
                    .await;
                }
                (CreateDetectionEvidence::from_detection(&detection), None)
            };
        evidence.candidate_headwords = input.headwords.clone();
        if let Some(confirmation) = &recovered_confirmation {
            let owner_bundle = serde_json::to_value(&evidence).map_err(serialization_error)?;
            if confirmation.binding.canonical_content_digest
                != canonical_headwords_digest(&input.headwords)?
                || confirmation.binding.owner_evidence_digest
                    != surface_owner_bundle_digest(&owner_bundle).map_err(serialization_error)?
                || confirmation.binding.normalization_version
                    != crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION
            {
                return Err(LexiconServiceError::DetectionMismatch);
            }
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
            &evidence.builtin_dictionary,
            evidence.entry_kind,
            evidence.matched_dialect,
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
            // The builtin dictionary is a static Kaikki snapshot, so a miss says
            // nothing about whether the term is real. Words and phrases alike stay
            // creatable by hand; only the headword evidence has to line up.
            (BuiltinDictionaryResultV2::NotFound, _, None) => {
                let WordHeadwordsV2::Unified { common } = &input.headwords else {
                    return Err(LexiconServiceError::DetectionMismatch);
                };
                if normalize_headword(common).map_err(map_headword_error)?.key
                    != evidence.normalized_headword
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
            _ => return Err(LexiconServiceError::DetectionMismatch),
        };
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
            detection_id: evidence.detection_id,
            request: evidence.request.clone(),
            normalized_headword: evidence.normalized_headword.clone(),
            entry_kind: evidence.entry_kind,
            matched_dialect,
            builtin_dictionary_status: builtin_status.to_owned(),
            smart_dictionary: WordDetectionSnapshotSmartDictionaryV2::Clear {
                surface_warning: None,
            },
            headwords: detected_headwords,
            suggested_pos,
            dictionary_provider,
            dictionary_coverage,
            dictionary_provenance,
            detected_at: now,
        };
        let mut word = AdminWordV2 {
            schema_version: 2,
            id: word_id,
            language: "en".to_owned(),
            kind: evidence.entry_kind,
            status: AdminWordStatus::Draft,
            revision: 1,
            lifecycle_revision: 1,
            published_revision: None,
            has_unpublished_changes: false,
            headwords: input.headwords.clone(),
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
        let surface_sources = crate::lexicon::repository::surface_projection_sources(&word)
            .map_err(surface_projection_error)?;
        let surface_keys =
            crate::lexicon::repository::surface_lock_keys([surface_sources.as_slice()]);
        LexiconRepository::lock_surface_policy_writer(&mut transaction)
            .await
            .map_err(repository_error)?;
        LexiconRepository::lock_surface_keys(&mut transaction, &surface_keys)
            .await
            .map_err(repository_error)?;
        let (current_matches, current_contexts) = self
            .headword_surface_matches_in_transaction(
                &mut transaction,
                &input.headwords,
                word.kind,
                None,
            )
            .await?;
        let projected_exact_entry_ids = current_matches
            .iter()
            .filter(|item| item.match_category == SurfaceMatchCategoryV2::ExactHeadword)
            .map(|item| item.existing.word_id)
            .collect::<Vec<_>>();
        let legacy_keys = normalized_headword_keys(&input.headwords)?;
        if LexiconRepository::has_unprojected_legacy_exact_in_transaction(
            &mut transaction,
            word.kind,
            &legacy_keys,
            &projected_exact_entry_ids,
        )
        .await
        .map_err(repository_error)?
        {
            return persist_create_failure(
                transaction,
                actor_id,
                idempotency_key,
                &request_hash,
                CreateIdempotentFailure::DuplicateWord,
            )
            .await;
        }
        let mut verified_surface = None;
        if !current_matches.is_empty() {
            let policy_name = surface_policy_name(&current_matches);
            let policy = self
                .surface_policies
                .policy(policy_name)
                .await
                .map_err(LexiconServiceError::SurfacePolicy)?;
            let owner_bundle = serde_json::to_value(&evidence).map_err(serialization_error)?;
            let expected = ExpectedSurfaceConfirmation {
                binding: SurfaceConfirmationBinding {
                    actor_id,
                    command: SurfaceConsumptionCommand::CreateEntry,
                    owner_context: input.detection_id.to_string(),
                    base_revision: None,
                    canonical_content_digest: canonical_headwords_digest(&input.headwords)?,
                    owner_evidence_digest: surface_owner_bundle_digest(&owner_bundle)
                        .map_err(serialization_error)?,
                    normalization_version:
                        crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
                    policy_name: policy.name,
                    policy_epoch: policy.epoch,
                },
                current_policy: policy,
            };

            let Some(token) = input.confirmed_surface_match_token.as_deref() else {
                let snapshot = match self
                    .create_detection_surface_snapshot(
                        actor_id,
                        &evidence,
                        current_matches,
                        current_contexts,
                        Some(policy),
                    )
                    .await
                {
                    Ok((_, snapshot)) => snapshot,
                    Err(LexiconServiceError::SurfaceSnapshot(
                        SurfaceSnapshotError::PolicyChanged(name),
                    )) => {
                        let current = self
                            .surface_policies
                            .policy(name)
                            .await
                            .map_err(LexiconServiceError::SurfacePolicy)?;
                        return persist_create_failure(
                            transaction,
                            actor_id,
                            idempotency_key,
                            &request_hash,
                            CreateIdempotentFailure::SurfacePolicyChanged { policy: current },
                        )
                        .await;
                    }
                    Err(error) => return Err(error),
                };
                let failure = if policy.enabled {
                    CreateIdempotentFailure::SurfaceMatchAcknowledgementRequired {
                        page: Box::new(snapshot.page),
                    }
                } else {
                    CreateIdempotentFailure::ExactHeadwordCreationTemporarilyDisabled {
                        page: Box::new(snapshot.page),
                    }
                };
                return persist_create_failure(
                    transaction,
                    actor_id,
                    idempotency_key,
                    &request_hash,
                    failure,
                )
                .await;
            };
            let confirmation = match self.surface_snapshots.verify(token, &expected).await {
                Ok(confirmation) => confirmation,
                Err(SurfaceSnapshotError::Expired) => {
                    return persist_create_failure(
                        transaction,
                        actor_id,
                        idempotency_key,
                        &request_hash,
                        CreateIdempotentFailure::SurfaceMatchSnapshotExpired,
                    )
                    .await;
                }
                Err(SurfaceSnapshotError::PolicyChanged(name)) => {
                    let current = self
                        .surface_policies
                        .policy(name)
                        .await
                        .map_err(LexiconServiceError::SurfacePolicy)?;
                    return persist_create_failure(
                        transaction,
                        actor_id,
                        idempotency_key,
                        &request_hash,
                        CreateIdempotentFailure::SurfacePolicyChanged { policy: current },
                    )
                    .await;
                }
                Err(SurfaceSnapshotError::BindingMismatch) => {
                    let snapshot = match self
                        .create_detection_surface_snapshot(
                            actor_id,
                            &evidence,
                            current_matches,
                            current_contexts,
                            Some(policy),
                        )
                        .await
                    {
                        Ok((_, snapshot)) => snapshot,
                        Err(LexiconServiceError::SurfaceSnapshot(
                            SurfaceSnapshotError::PolicyChanged(name),
                        )) => {
                            let current = self
                                .surface_policies
                                .policy(name)
                                .await
                                .map_err(LexiconServiceError::SurfacePolicy)?;
                            return persist_create_failure(
                                transaction,
                                actor_id,
                                idempotency_key,
                                &request_hash,
                                CreateIdempotentFailure::SurfacePolicyChanged { policy: current },
                            )
                            .await;
                        }
                        Err(error) => return Err(error),
                    };
                    return persist_create_failure(
                        transaction,
                        actor_id,
                        idempotency_key,
                        &request_hash,
                        CreateIdempotentFailure::SurfaceMatchesChanged {
                            page: Box::new(snapshot.page),
                        },
                    )
                    .await;
                }
                Err(error) => return Err(LexiconServiceError::SurfaceSnapshot(error)),
            };
            let contexts_changed = surface_context_digest(&current_contexts)
                .map_err(LexiconServiceError::SurfaceSnapshot)?
                != confirmation.context_digest;
            if contexts_changed
                || surface_match_ids_changed(
                    current_matches.iter().map(|item| item.match_id.as_str()),
                    confirmation.match_ids.iter().map(String::as_str),
                )
            {
                let snapshot = match self
                    .create_detection_surface_snapshot(
                        actor_id,
                        &evidence,
                        current_matches,
                        current_contexts,
                        Some(policy),
                    )
                    .await
                {
                    Ok((_, snapshot)) => snapshot,
                    Err(LexiconServiceError::SurfaceSnapshot(
                        SurfaceSnapshotError::PolicyChanged(name),
                    )) => {
                        let current = self
                            .surface_policies
                            .policy(name)
                            .await
                            .map_err(LexiconServiceError::SurfacePolicy)?;
                        return persist_create_failure(
                            transaction,
                            actor_id,
                            idempotency_key,
                            &request_hash,
                            CreateIdempotentFailure::SurfacePolicyChanged { policy: current },
                        )
                        .await;
                    }
                    Err(error) => return Err(error),
                };
                return persist_create_failure(
                    transaction,
                    actor_id,
                    idempotency_key,
                    &request_hash,
                    CreateIdempotentFailure::SurfaceMatchesChanged {
                        page: Box::new(snapshot.page),
                    },
                )
                .await;
            }
            word.detection_snapshot.smart_dictionary =
                WordDetectionSnapshotSmartDictionaryV2::Warning {
                    surface_warning: surface_warning_audit(
                        &confirmation,
                        &current_matches,
                        &current_contexts,
                        actor_id,
                    ),
                };
            verified_surface = Some(confirmation);
        }
        if !LexiconRepository::consume_detection(
            &mut transaction,
            actor_id,
            input.detection_id,
            word_id,
        )
        .await
        .map_err(repository_error)?
        {
            return persist_create_failure(
                transaction,
                actor_id,
                idempotency_key,
                &request_hash,
                CreateIdempotentFailure::DetectionExpired,
            )
            .await;
        }
        if let Err(error) = LexiconRepository::insert_entry(
            &mut transaction,
            &word,
            actor_id,
            request_id,
            &part_map,
            idempotency_key,
            &request_hash,
        )
        .await
        {
            if matches!(error, LexiconRepositoryError::DuplicateHeadword) {
                transaction.rollback().await.map_err(database_error)?;
                return persist_create_failure_after_rollback(
                    self.repository.pool(),
                    actor_id,
                    idempotency_key,
                    &request_hash,
                    CreateIdempotentFailure::DuplicateWord,
                )
                .await;
            }
            return Err(repository_error(error));
        }
        if let (
            Some(confirmation),
            WordDetectionSnapshotSmartDictionaryV2::Warning { surface_warning },
        ) = (&verified_surface, &word.detection_snapshot.smart_dictionary)
        {
            LexiconRepository::insert_surface_acknowledgement(
                &mut transaction,
                &word,
                surface_warning,
                &canonical_headwords_digest(&word.headwords)?,
                &confirmation.match_ids,
            )
            .await
            .map_err(repository_error)?;
        }
        LexiconRepository::replace_surface_projection(
            &mut transaction,
            word.id,
            word.revision,
            crate::lexicon::repository::SurfaceContentScope::Draft,
            None,
            &[],
            &surface_sources,
        )
        .await
        .map_err(repository_error)?;
        transaction.commit().await.map_err(database_error)?;

        if let Some(confirmation) = &verified_surface
            && let Err(error) = self.surface_snapshots.remove_verified(confirmation).await
        {
            tracing::warn!(%error, snapshot_id = %confirmation.snapshot_id, "created lexicon entry but failed to remove surface confirmation");
        }

        if let Err(error) = self.detections.remove(actor_id, input.detection_id).await {
            tracing::warn!(%error, detection_id = %input.detection_id, "created lexicon entry but failed to remove detection context");
        }
        Ok(AdminWordV2Envelope { word })
    }
}

// --- support ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CreateDetectionEvidence {
    detection_id: Uuid,
    expires_at: chrono::DateTime<Utc>,
    request: DetectionRequestEcho,
    normalized_headword: String,
    entry_kind: EntryKind,
    matched_dialect: Option<Dialect>,
    builtin_dictionary: BuiltinDictionaryResultV2,
    candidate_headwords: WordHeadwordsV2,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "failure", rename_all = "snake_case")]
enum CreateIdempotentFailure {
    DetectionExpired,
    DuplicateWord,
    SurfaceMatchAcknowledgementRequired { page: Box<SurfaceMatchPageV2> },
    SurfaceMatchesChanged { page: Box<SurfaceMatchPageV2> },
    SurfaceMatchSnapshotExpired,
    SurfacePolicyChanged { policy: SurfaceCreationPolicy },
    ExactHeadwordCreationTemporarilyDisabled { page: Box<SurfaceMatchPageV2> },
}

impl CreateIdempotentFailure {
    const fn status(&self) -> i16 {
        match self {
            Self::DetectionExpired | Self::SurfaceMatchSnapshotExpired => 410,
            Self::DuplicateWord
            | Self::SurfaceMatchAcknowledgementRequired { .. }
            | Self::SurfaceMatchesChanged { .. }
            | Self::SurfacePolicyChanged { .. }
            | Self::ExactHeadwordCreationTemporarilyDisabled { .. } => 409,
        }
    }

    fn into_service_error(self) -> LexiconServiceError {
        match self {
            Self::DetectionExpired => LexiconServiceError::DetectionExpired,
            Self::DuplicateWord => LexiconServiceError::DuplicateWord,
            Self::SurfaceMatchAcknowledgementRequired { page } => {
                LexiconServiceError::SurfaceMatchAcknowledgementRequired(page)
            }
            Self::SurfaceMatchesChanged { page } => {
                LexiconServiceError::SurfaceMatchesChanged(page)
            }
            Self::SurfaceMatchSnapshotExpired => LexiconServiceError::SurfaceMatchSnapshotExpired,
            Self::SurfacePolicyChanged { policy } => {
                LexiconServiceError::SurfacePolicyChanged(policy)
            }
            Self::ExactHeadwordCreationTemporarilyDisabled { page } => {
                LexiconServiceError::ExactHeadwordCreationTemporarilyDisabled(page)
            }
        }
    }
}

async fn persist_create_failure(
    mut transaction: sqlx::Transaction<'_, sqlx::Postgres>,
    actor_id: Uuid,
    idempotency_key: Uuid,
    request_hash: &[u8],
    failure: CreateIdempotentFailure,
) -> Result<AdminWordV2Envelope, LexiconServiceError> {
    LexiconRepository::insert_create_idempotency_failure(
        &mut transaction,
        actor_id,
        idempotency_key,
        request_hash,
        failure.status(),
        serde_json::to_value(&failure).map_err(serialization_error)?,
    )
    .await
    .map_err(repository_error)?;
    transaction.commit().await.map_err(database_error)?;
    Err(failure.into_service_error())
}

async fn persist_create_failure_after_rollback(
    pool: &sqlx::PgPool,
    actor_id: Uuid,
    idempotency_key: Uuid,
    request_hash: &[u8],
    failure: CreateIdempotentFailure,
) -> Result<AdminWordV2Envelope, LexiconServiceError> {
    let mut transaction = pool.begin().await.map_err(database_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("{CREATE_SCOPE}:{actor_id}:{idempotency_key}"))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    if let Some(existing) =
        LexiconRepository::idempotency(&mut transaction, CREATE_SCOPE, actor_id, idempotency_key)
            .await
            .map_err(repository_error)?
    {
        if existing.request_hash != request_hash {
            return Err(LexiconServiceError::IdempotencyConflict);
        }
        if existing.response_status >= 400 {
            let replay: CreateIdempotentFailure =
                serde_json::from_value(existing.response_body).map_err(serialization_error)?;
            transaction.commit().await.map_err(database_error)?;
            return Err(replay.into_service_error());
        }
        transaction.commit().await.map_err(database_error)?;
        return serde_json::from_value(existing.response_body).map_err(serialization_error);
    }
    persist_create_failure(
        transaction,
        actor_id,
        idempotency_key,
        request_hash,
        failure,
    )
    .await
}

impl CreateDetectionEvidence {
    fn from_detection(detection: &DetectWordResponseV2) -> Self {
        let candidate_headwords = match &detection.builtin_dictionary {
            BuiltinDictionaryResultV2::Matched { headwords, .. } => headwords.clone(),
            BuiltinDictionaryResultV2::NotFound | BuiltinDictionaryResultV2::Unavailable { .. } => {
                WordHeadwordsV2::Unified {
                    common: detection.request.headword.clone(),
                }
            }
        };
        Self {
            detection_id: detection.detection_id,
            expires_at: detection.expires_at,
            request: detection.request.clone(),
            normalized_headword: detection.normalized_headword.clone(),
            entry_kind: detection.entry_kind,
            matched_dialect: detection.matched_dialect,
            builtin_dictionary: detection.builtin_dictionary.clone(),
            candidate_headwords,
        }
    }
}

#[derive(Debug, Clone)]
struct HeadwordSurfaceCandidate {
    candidate_ref: String,
    surface: String,
    normalized_surface: String,
    dialect: Dialect,
    entry_kind: EntryKind,
    lookup_keys: Vec<crate::lexicon::model::SurfaceLookupKey>,
}

impl LexiconService {
    async fn headword_surface_matches(
        &self,
        headwords: &WordHeadwordsV2,
        entry_kind: EntryKind,
        excluding_entry_id: Option<Uuid>,
    ) -> Result<(Vec<LexiconSurfaceMatchV2>, Vec<MatchedEntryContextV2>), LexiconServiceError> {
        let candidates = headword_surface_candidates(headwords, entry_kind)?;
        let requested = candidates
            .iter()
            .flat_map(|candidate| candidate.lookup_keys.iter().cloned())
            .collect::<Vec<_>>();
        let sources = self
            .repository
            .surface_sources("en", &requested, excluding_entry_id)
            .await
            .map_err(repository_error)?;
        let mut matches = BTreeMap::new();
        for candidate in &candidates {
            for source in sources.iter().filter(|source| {
                source.normalized_surface == candidate.normalized_surface
                    && candidate.lookup_keys.iter().any(|key| {
                        key.dialect_scope == source.matched_dialect_scope
                            && key.normalized_surface == source.normalized_surface
                    })
            }) {
                let item = surface_match(candidate, source)?;
                matches.entry(item.match_id.clone()).or_insert(item);
            }
        }
        let mut matches = matches.into_values().collect::<Vec<_>>();
        let entry_ids = matched_entry_ids(&matches);
        let inbound = inbound_relation_previews(
            &self
                .repository
                .surface_inbound_relations(&entry_ids)
                .await
                .map_err(repository_error)?,
        )?;
        matches.extend(relation_surface_matches(&matches, &inbound)?);
        let records = self
            .repository
            .surface_entry_contexts(&entry_ids)
            .await
            .map_err(repository_error)?;
        let contexts = surface_contexts_from_records(records, &inbound)?;
        Ok((matches, contexts))
    }

    pub(super) async fn surface_match_contexts(
        &self,
        matches: &[LexiconSurfaceMatchV2],
    ) -> Result<Vec<MatchedEntryContextV2>, LexiconServiceError> {
        let entry_ids = matched_entry_ids(matches);
        let records = self
            .repository
            .surface_entry_contexts(&entry_ids)
            .await
            .map_err(repository_error)?;
        let inbound = self
            .repository
            .surface_inbound_relations(&entry_ids)
            .await
            .map_err(repository_error)?;
        surface_contexts_from_records(records, &inbound_relation_previews(&inbound)?)
    }

    pub(super) async fn headword_surface_matches_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        headwords: &WordHeadwordsV2,
        entry_kind: EntryKind,
        excluding_entry_id: Option<Uuid>,
    ) -> Result<(Vec<LexiconSurfaceMatchV2>, Vec<MatchedEntryContextV2>), LexiconServiceError> {
        let candidates = headword_surface_candidates(headwords, entry_kind)?;
        let requested = candidates
            .iter()
            .flat_map(|candidate| candidate.lookup_keys.iter().cloned())
            .collect::<Vec<_>>();
        let sources = LexiconRepository::surface_sources_in_transaction(
            tx,
            "en",
            &requested,
            excluding_entry_id,
        )
        .await
        .map_err(repository_error)?;
        let mut matches = BTreeMap::new();
        for candidate in &candidates {
            for source in sources.iter().filter(|source| {
                source.normalized_surface == candidate.normalized_surface
                    && candidate.lookup_keys.iter().any(|key| {
                        key.dialect_scope == source.matched_dialect_scope
                            && key.normalized_surface == source.normalized_surface
                    })
            }) {
                let item = surface_match(candidate, source)?;
                matches.entry(item.match_id.clone()).or_insert(item);
            }
        }
        let mut matches = matches.into_values().collect::<Vec<_>>();
        let entry_ids = matched_entry_ids(&matches);
        LexiconRepository::lock_surface_contexts(tx, &entry_ids)
            .await
            .map_err(repository_error)?;
        let records = LexiconRepository::surface_entry_contexts_in_transaction(tx, &entry_ids)
            .await
            .map_err(repository_error)?;
        let inbound = inbound_relation_previews(
            &LexiconRepository::surface_inbound_relations_in_transaction(tx, &entry_ids)
                .await
                .map_err(repository_error)?,
        )?;
        matches.extend(relation_surface_matches(&matches, &inbound)?);
        let contexts = surface_contexts_from_records(records, &inbound)?;
        Ok((matches, contexts))
    }

    async fn create_detection_surface_snapshot(
        &self,
        actor_id: Uuid,
        evidence: &CreateDetectionEvidence,
        items: Vec<LexiconSurfaceMatchV2>,
        contexts: Vec<MatchedEntryContextV2>,
        locked_policy: Option<SurfaceCreationPolicy>,
    ) -> Result<(SurfaceCreationPolicy, CreatedSurfaceSnapshot), LexiconServiceError> {
        let policy_name = surface_policy_name(&items);
        let policy = match locked_policy {
            Some(policy) if policy.name == policy_name => policy,
            Some(_) => return Err(invariant_record()),
            None => self
                .surface_policies
                .policy(policy_name)
                .await
                .map_err(LexiconServiceError::SurfacePolicy)?,
        };
        if !policy.enabled && policy.name != SurfacePolicyNameV2::AllowNewExactHeadwordEntries {
            return Err(LexiconServiceError::Repository(
                LexiconRepositoryError::Invariant("ordinary surface warning policy is disabled"),
            ));
        }
        let owner_bundle = serde_json::to_value(evidence).map_err(serialization_error)?;
        let binding = SurfaceConfirmationBinding {
            actor_id,
            command: SurfaceConsumptionCommand::CreateEntry,
            owner_context: evidence.detection_id.to_string(),
            base_revision: None,
            canonical_content_digest: canonical_headwords_digest(&evidence.candidate_headwords)?,
            owner_evidence_digest: surface_owner_bundle_digest(&owner_bundle)
                .map_err(serialization_error)?,
            normalization_version: crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
            policy_name: policy.name,
            policy_epoch: policy.epoch,
        };
        let snapshot = self
            .surface_snapshots
            .create(CreateSurfaceSnapshot {
                binding,
                policy_enabled: policy.enabled,
                policy_block_code: (!policy.enabled)
                    .then_some(SurfacePolicyBlockCodeV2::ExactHeadwordCreationTemporarilyDisabled),
                items,
                matched_entry_contexts: contexts,
                confirmation_reasons: vec![
                    SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches,
                ],
                owner_bundle,
                page_size: DEFAULT_SURFACE_PAGE_SIZE,
            })
            .await
            .map_err(LexiconServiceError::SurfaceSnapshot)?;
        Ok((policy, snapshot))
    }
}

pub(super) fn surface_contexts_from_records(
    records: Vec<crate::lexicon::model::SurfaceEntryContextRecord>,
    inbound: &[InboundRelationPreview],
) -> Result<Vec<MatchedEntryContextV2>, LexiconServiceError> {
    let mut relation_summaries = HashMap::<Uuid, RelationSummaryBuilder>::new();
    for reference in inbound {
        relation_summaries
            .entry(reference.target_entry_id)
            .or_default()
            .push(&reference.preview);
    }
    records
        .into_iter()
        .map(|record| {
            let forms: DraftFormsStepContent =
                serde_json::from_value(record.forms).map_err(serialization_error)?;
            let meanings: DraftMeaningsStepContent =
                serde_json::from_value(record.meanings).map_err(serialization_error)?;
            let mut pos_labels = forms
                .pos
                .iter()
                .map(|pos| pos.pos.clone())
                .collect::<Vec<_>>();
            pos_labels.sort();
            pos_labels.dedup();
            pos_labels.truncate(5);
            let mut gloss_previews = meanings
                .pos
                .iter()
                .flat_map(|pos| pos.senses.iter())
                .map(published_sense_gloss)
                .filter(|gloss| !gloss.is_empty())
                .take(5)
                .collect::<Vec<_>>();
            gloss_previews.dedup();
            Ok(MatchedEntryContextV2 {
                word_id: record.entry_id,
                pos_labels,
                gloss_previews,
                updated_at: record.updated_at,
                inbound_relations: relation_summaries
                    .remove(&record.entry_id)
                    .unwrap_or_default()
                    .finish(),
            })
        })
        .collect()
}

#[derive(Default)]
struct RelationSummaryBuilder {
    synonym: u32,
    antonym: u32,
    derivative: u32,
    total: u32,
    previews: Vec<RelationReferencePreviewV2>,
}

impl RelationSummaryBuilder {
    fn push(&mut self, preview: &RelationReferencePreviewV2) {
        self.total = self.total.saturating_add(1);
        match preview.relation {
            RelationTypeV2::Synonym => self.synonym = self.synonym.saturating_add(1),
            RelationTypeV2::Antonym => self.antonym = self.antonym.saturating_add(1),
            RelationTypeV2::Derivative => self.derivative = self.derivative.saturating_add(1),
        }
        // 只有真正留下来的前 5 条才值得克隆——计数用不着所有权。
        if self.previews.len() < 5 {
            self.previews.push(preview.clone());
        }
    }

    fn finish(self) -> RelationReferenceSummaryV2 {
        RelationReferenceSummaryV2 {
            total: self.total,
            by_type: RelationReferenceCountsV2 {
                synonym: self.synonym,
                antonym: self.antonym,
                derivative: self.derivative,
            },
            truncated: self.total as usize > self.previews.len(),
            previews: self.previews,
        }
    }
}

fn headword_surface_candidates(
    headwords: &WordHeadwordsV2,
    entry_kind: EntryKind,
) -> Result<Vec<HeadwordSurfaceCandidate>, LexiconServiceError> {
    let values = match headwords {
        WordHeadwordsV2::Unified { common } => vec![("headword:common", common, Dialect::Common)],
        WordHeadwordsV2::Distinguish { uk, us, .. } => vec![
            ("headword:uk", uk, Dialect::Uk),
            ("headword:us", us, Dialect::Us),
        ],
    };
    values
        .into_iter()
        .map(|(candidate_ref, surface, dialect)| {
            let scopes = crate::lexicon::surface::normalize_surface_scopes(surface, dialect)
                .map_err(surface_projection_error)?;
            let normalized_surface = scopes
                .first()
                .ok_or_else(invariant_record)?
                .normalized_surface
                .clone();
            Ok(HeadwordSurfaceCandidate {
                candidate_ref: candidate_ref.to_owned(),
                surface: scopes[0].surface.clone(),
                normalized_surface,
                dialect,
                entry_kind,
                lookup_keys: scopes
                    .into_iter()
                    .map(|scope| crate::lexicon::model::SurfaceLookupKey {
                        dialect_scope: scope.dialect_scope.to_owned(),
                        normalized_surface: scope.normalized_surface,
                    })
                    .collect(),
            })
        })
        .collect()
}

fn surface_match(
    candidate: &HeadwordSurfaceCandidate,
    source: &crate::lexicon::model::SurfaceSourceRecord,
) -> Result<LexiconSurfaceMatchV2, LexiconServiceError> {
    let (existing_kind, existing) = existing_surface_match(source)?;
    let category = if source.source_kind == "form" {
        SurfaceMatchCategoryV2::HeadwordForm
    } else if existing_kind == candidate.entry_kind {
        SurfaceMatchCategoryV2::ExactHeadword
    } else {
        SurfaceMatchCategoryV2::CrossKindHeadword
    };
    let candidate_wire = SurfaceMatchCandidateV2::Headword {
        candidate_ref: candidate.candidate_ref.clone(),
        candidate_word_id: None,
        surface: candidate.surface.clone(),
        normalized_surface: candidate.normalized_surface.clone(),
        dialect: candidate.dialect,
        entry_kind: candidate.entry_kind,
    };
    let match_id = crate::platform::hash_token(
        &serde_json::to_string(&serde_json::json!({
            "candidate_ref": candidate.candidate_ref,
            "candidate_surface": candidate.normalized_surface,
            "candidate_dialect": candidate.dialect,
            "existing": existing,
            "normalization_version": source.normalization_version,
        }))
        .map_err(serialization_error)?,
    );
    Ok(LexiconSurfaceMatchV2 {
        match_id,
        match_category: category,
        severity: SurfaceMatchSeverityV2::Warning,
        attention_level: if category == SurfaceMatchCategoryV2::ExactHeadword {
            SurfaceAttentionLevelV2::High
        } else {
            SurfaceAttentionLevelV2::Normal
        },
        can_continue: SurfaceCanContinueTrue,
        confirmation_reasons: vec![SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches],
        candidate: candidate_wire,
        existing,
    })
}

/// 一条已解析的入站关联，附带它落在草稿还是当前发布。
pub(super) struct InboundRelationPreview {
    target_entry_id: Uuid,
    source_node_id: Uuid,
    content_scope: SurfaceContentScopeV2,
    preview: RelationReferencePreviewV2,
}

/// 把入站关联记录一次性解析成 preview，供关联词命中行与命中词条上下文摘要共用。
///
/// current_publication 那一路的 `inbound_relation_preview` 要克隆整份发布快照再
/// 反序列化成 `AdminWordV2` 才能取到关系类型，解两遍代价明显，所以两个消费方共用
/// 同一份结果。
pub(super) fn inbound_relation_previews(
    inbound: &[crate::lexicon::model::SurfaceInboundRelationRecord],
) -> Result<Vec<InboundRelationPreview>, LexiconServiceError> {
    let mut previews = Vec::with_capacity(inbound.len());
    for reference in inbound {
        let Some(preview) = inbound_relation_preview(reference)? else {
            continue;
        };
        previews.push(InboundRelationPreview {
            target_entry_id: reference.target_entry_id,
            source_node_id: reference.source_node_id,
            content_scope: if reference.draft_relation_type.is_some() {
                SurfaceContentScopeV2::Draft
            } else {
                SurfaceContentScopeV2::CurrentPublication
            },
            preview,
        });
    }
    Ok(previews)
}

const fn relation_rank(relation: RelationTypeV2) -> u8 {
    match relation {
        RelationTypeV2::Synonym => 0,
        RelationTypeV2::Antonym => 1,
        RelationTypeV2::Derivative => 2,
    }
}

fn matched_entry_ids(matches: &[LexiconSurfaceMatchV2]) -> Vec<Uuid> {
    let mut entry_ids = matches
        .iter()
        .map(|item| item.existing.word_id)
        .collect::<Vec<_>>();
    entry_ids.sort_unstable();
    entry_ids.dedup();
    entry_ids
}

/// 按命中词条汇总入站关联词。
///
/// 同一个引用方词条可能在多个义项里用同一种关系指向同一个目标；对管理员来说
/// 「被 X 引用为近义词」说一次就够，因此按 (引用方, 关系类型) 去重并保留最小的
/// 关联词节点 id，保证同一份数据每次得到同一个 `match_id`。
///
/// 不额外截断：命中条目本来就由 `SurfaceMatchPageBaseV2` 分页承载，主词与词形
/// 命中同样不设上限，这里加一道私有上限只会让命中行与
/// `MatchedEntryContextV2.inbound_relations.previews` 各自截出不同的子集。
fn inbound_relation_matches(
    inbound: &[InboundRelationPreview],
) -> HashMap<Uuid, Vec<&InboundRelationPreview>> {
    let mut deduped = BTreeMap::<(Uuid, Uuid, u8), &InboundRelationPreview>::new();
    for reference in inbound {
        let key = (
            reference.target_entry_id,
            reference.preview.source_word_id,
            relation_rank(reference.preview.relation),
        );
        match deduped.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(reference);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if reference.source_node_id < entry.get().source_node_id {
                    entry.insert(reference);
                }
            }
        }
    }
    let mut by_target = HashMap::<Uuid, Vec<&InboundRelationPreview>>::new();
    for ((target_entry_id, _, _), item) in deduped {
        by_target.entry(target_entry_id).or_default().push(item);
    }
    by_target
}

/// 关联词维度：给已经因主词命中的词条补上「谁把它引用为关联词」这条命中原因。
///
/// `lexicon.relations` 的目标受外键约束必须是已存在词条的义项，所以关联词永远
/// 不会引入新的词面。它因此只从主词命中派生，既不需要单独进
/// `lexicon.surface_sources` 投影，也不会把原本 clear 的检测变成 warning，更不会
/// 影响 `surface_policy_name`（那里只看 `ExactHeadword`）。
fn relation_surface_matches(
    headword_matches: &[LexiconSurfaceMatchV2],
    inbound: &[InboundRelationPreview],
) -> Result<Vec<LexiconSurfaceMatchV2>, LexiconServiceError> {
    let by_target = inbound_relation_matches(inbound);
    if by_target.is_empty() {
        return Ok(Vec::new());
    }
    // 「谁引用了这个词条」与本次录入撞上的是哪一侧词头无关，因此每个命中词条只挂
    // 一次：distinguish 录入的 uk / us 两个候选、以及同一词条的 draft 与
    // current_publication 两条主词行，都归并到同一个代表，否则同一句话会重复出现。
    // 代表按 (candidate_ref, match_id) 取最小，保证同一份数据每次得到同一个 match_id。
    let mut ranked = headword_matches
        .iter()
        .filter_map(|item| {
            let SurfaceMatchCandidateV2::Headword { candidate_ref, .. } = &item.candidate else {
                return None;
            };
            matches!(
                item.existing.source,
                ExistingSurfaceSourceV2::Headword { .. }
            )
            .then_some((candidate_ref.as_str(), item.match_id.as_str(), item))
        })
        .collect::<Vec<_>>();
    ranked.sort_unstable_by_key(|(candidate_ref, match_id, _)| (*candidate_ref, *match_id));
    let mut representatives = BTreeMap::<Uuid, &LexiconSurfaceMatchV2>::new();
    for (_, _, item) in ranked {
        representatives.entry(item.existing.word_id).or_insert(item);
    }

    let mut matches = Vec::new();
    for (word_id, parent) in representatives {
        let Some(relations) = by_target.get(&word_id) else {
            continue;
        };
        let ExistingSurfaceSourceV2::Headword {
            surface, dialect, ..
        } = &parent.existing.source
        else {
            return Err(invariant_record());
        };
        for relation in relations {
            let existing = ExistingSurfaceMatchV2 {
                word_id: parent.existing.word_id,
                headword: parent.existing.headword.clone(),
                kind: parent.existing.kind,
                status: parent.existing.status,
                source: ExistingSurfaceSourceV2::Relation {
                    source_id: format!(
                        "entry:{}:relation:{}",
                        relation.preview.source_word_id, relation.source_node_id
                    ),
                    source_node_id: relation.source_node_id,
                    content_scope: relation.content_scope,
                    surface: surface.clone(),
                    dialect: *dialect,
                    relation_type: relation.preview.relation,
                    referencing_word_id: relation.preview.source_word_id,
                    referencing_headword: relation.preview.source_headword.clone(),
                    referencing_status: relation.preview.source_status,
                },
            };
            let match_id = crate::platform::hash_token(
                &serde_json::to_string(&serde_json::json!({
                    "headword_match_id": parent.match_id,
                    "existing": existing,
                }))
                .map_err(serialization_error)?,
            );
            matches.push(LexiconSurfaceMatchV2 {
                match_id,
                match_category: SurfaceMatchCategoryV2::HeadwordRelation,
                severity: SurfaceMatchSeverityV2::Warning,
                attention_level: SurfaceAttentionLevelV2::Normal,
                can_continue: SurfaceCanContinueTrue,
                confirmation_reasons: vec![
                    SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches,
                ],
                candidate: parent.candidate.clone(),
                existing,
            });
        }
    }
    Ok(matches)
}

pub(super) fn existing_surface_match(
    source: &crate::lexicon::model::SurfaceSourceRecord,
) -> Result<(EntryKind, ExistingSurfaceMatchV2), LexiconServiceError> {
    let existing_kind = parse_kind(&source.entry_kind).ok_or_else(invariant_record)?;
    let existing_status = parse_surface_status(&source.lifecycle_status)?;
    let existing_dialect = parse_dialect(&source.dialect).ok_or_else(invariant_record)?;
    let content_scope = match source.content_scope.as_str() {
        "draft" => SurfaceContentScopeV2::Draft,
        "current_publication" => SurfaceContentScopeV2::CurrentPublication,
        _ => return Err(invariant_record()),
    };
    let existing_source = match source.source_kind.as_str() {
        "headword" => ExistingSurfaceSourceV2::Headword {
            source_id: source.source_id.clone(),
            content_scope,
            surface: source.surface.clone(),
            dialect: existing_dialect,
        },
        "form" => ExistingSurfaceSourceV2::Form {
            source_id: source.source_id.clone(),
            source_node_id: source.source_node_id.ok_or_else(invariant_record)?,
            content_scope,
            surface: source.surface.clone(),
            dialect: existing_dialect,
            pos_id: source.pos_id.ok_or_else(invariant_record)?,
            pos: source.pos.clone().ok_or_else(invariant_record)?,
            form_type: WordFormTypeV2::try_from(
                source.form_type.as_deref().ok_or_else(invariant_record)?,
            )
            .map_err(|()| invariant_record())?,
        },
        _ => return Err(invariant_record()),
    };
    let existing = ExistingSurfaceMatchV2 {
        word_id: source.entry_id,
        headword: source.entry_headword.clone(),
        kind: existing_kind,
        status: existing_status,
        source: existing_source,
    };
    Ok((existing_kind, existing))
}

pub(super) fn parse_surface_status(value: &str) -> Result<AdminWordStatus, LexiconServiceError> {
    match value {
        "draft" => Ok(AdminWordStatus::Draft),
        "published" => Ok(AdminWordStatus::Published),
        "archived" => Ok(AdminWordStatus::Archived),
        _ => Err(invariant_record()),
    }
}

fn surface_policy_name(items: &[LexiconSurfaceMatchV2]) -> SurfacePolicyNameV2 {
    if items
        .iter()
        .any(|item| item.match_category == SurfaceMatchCategoryV2::ExactHeadword)
    {
        SurfacePolicyNameV2::AllowNewExactHeadwordEntries
    } else {
        SurfacePolicyNameV2::SurfaceWarningAcknowledgement
    }
}

fn has_unprojected_legacy_exact(
    legacy: &[crate::lexicon::model::DuplicateRecord],
    matches: &[LexiconSurfaceMatchV2],
) -> bool {
    let projected_entry_ids = matches
        .iter()
        .filter(|item| item.match_category == SurfaceMatchCategoryV2::ExactHeadword)
        .map(|item| item.existing.word_id)
        .collect::<std::collections::HashSet<_>>();
    legacy
        .iter()
        .any(|record| !projected_entry_ids.contains(&record.entry_id))
}

pub(super) fn canonical_headwords_digest(
    headwords: &WordHeadwordsV2,
) -> Result<String, LexiconServiceError> {
    Ok(crate::platform::hash_token(
        &serde_json::to_string(headwords).map_err(serialization_error)?,
    ))
}

fn relation_type_for_node(word: &AdminWordV2, node_id: Uuid) -> Option<RelationTypeV2> {
    let relation = word
        .meanings
        .pos
        .iter()
        .flat_map(|pos| pos.senses.iter())
        .flat_map(|sense| sense.relations.iter())
        .find(|relation| relation.id == node_id)?;
    parse_relation_type(&relation.relation)
}

fn parse_relation_type(value: &str) -> Option<RelationTypeV2> {
    match value {
        "synonym" => Some(RelationTypeV2::Synonym),
        "antonym" => Some(RelationTypeV2::Antonym),
        "derivative" => Some(RelationTypeV2::Derivative),
        _ => None,
    }
}

fn inbound_relation_preview(
    reference: &crate::lexicon::model::SurfaceInboundRelationRecord,
) -> Result<Option<RelationReferencePreviewV2>, LexiconServiceError> {
    let relation = if let Some(relation) = reference.draft_relation_type.as_deref() {
        parse_relation_type(relation)
    } else if let Some(snapshot) = reference.source_snapshot.as_ref() {
        let source: AdminWordV2 =
            serde_json::from_value(snapshot.clone()).map_err(serialization_error)?;
        relation_type_for_node(&source, reference.source_node_id)
    } else {
        None
    };
    let Some(relation) = relation else {
        return Ok(None);
    };
    let source_headword = match reference.source_headword_mode.as_str() {
        "unified" => reference.source_common_headword.clone(),
        "distinguish" => match reference.source_dialect.as_deref() {
            Some("uk") => reference
                .source_uk_headword
                .as_ref()
                .zip(reference.source_us_headword.as_ref()),
            Some("us") => reference
                .source_us_headword
                .as_ref()
                .zip(reference.source_uk_headword.as_ref()),
            _ => None,
        }
        .map(|(first, second)| format!("{first} / {second}")),
        _ => None,
    }
    .ok_or_else(invariant_record)?;
    Ok(Some(RelationReferencePreviewV2 {
        source_word_id: reference.source_entry_id,
        source_headword,
        source_status: parse_surface_status(&reference.source_status)?,
        relation,
    }))
}

fn surface_warning_audit(
    confirmation: &VerifiedSurfaceConfirmation,
    current_matches: &[LexiconSurfaceMatchV2],
    current_contexts: &[MatchedEntryContextV2],
    actor_id: Uuid,
) -> DetectionSurfaceWarningAuditV2 {
    // 这份 preview 会随 detection_snapshot 永久落进 lexicon.entries 与发布快照。
    // 写入 headword_relation 会让回退到上一版二进制的实例读不出这些词条——旧的
    // SurfaceMatchCategoryV2 没有该取值，也没有未知值兜底，entry_from_record 会硬失败。
    // 关联词命中本就只是对词面冲突的补充说明，审计只留冲突本身；完整命中集合仍由
    // total / match_digest / truncated 承载。
    let mut preview = current_matches
        .iter()
        .filter(|item| item.match_category != SurfaceMatchCategoryV2::HeadwordRelation)
        .take(5)
        .map(|item| {
            let context = current_contexts
                .iter()
                .find(|context| context.word_id == item.existing.word_id);
            let existing_dialect = match &item.existing.source {
                ExistingSurfaceSourceV2::Headword { dialect, .. }
                | ExistingSurfaceSourceV2::Form { dialect, .. }
                | ExistingSurfaceSourceV2::Relation { dialect, .. } => *dialect,
            };
            DetectionSurfaceMatchPreviewV2 {
                match_id: item.match_id.clone(),
                match_category: item.match_category,
                existing_word_id: item.existing.word_id,
                existing_headword: item.existing.headword.clone(),
                existing_kind: item.existing.kind,
                existing_status: item.existing.status,
                existing_dialect,
                pos_labels: context.map_or_else(Vec::new, |item| item.pos_labels.clone()),
                gloss_previews: context.map_or_else(Vec::new, |item| item.gloss_previews.clone()),
            }
        })
        .collect::<Vec<_>>();
    preview.sort_by(|left, right| left.match_id.cmp(&right.match_id));
    DetectionSurfaceWarningAuditV2 {
        total: confirmation.match_ids.len() as u64,
        match_digest: confirmation.match_digest.clone(),
        acknowledged: AcknowledgedTrue,
        acknowledged_at: Utc::now(),
        acknowledged_by: actor_id,
        policy_name: confirmation.binding.policy_name,
        policy_epoch: confirmation.binding.policy_epoch,
        truncated: confirmation.match_ids.len() > preview.len(),
        preview,
    }
}

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

fn surface_match_ids_changed<'a, I, J>(current_match_ids: I, confirmed_match_ids: J) -> bool
where
    I: IntoIterator<Item = &'a str>,
    J: IntoIterator<Item = &'a str>,
{
    let current_ids = current_match_ids
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let confirmed_ids = confirmed_match_ids
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    current_ids != confirmed_ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(source_kind: &str, entry_kind: &str) -> crate::lexicon::model::SurfaceSourceRecord {
        let is_form = source_kind == "form";
        crate::lexicon::model::SurfaceSourceRecord {
            matched_dialect_scope: "uk".to_owned(),
            entry_id: Uuid::now_v7(),
            entry_headword: "workspace".to_owned(),
            entry_headword_dialect: "common".to_owned(),
            entry_kind: entry_kind.to_owned(),
            lifecycle_status: "draft".to_owned(),
            source_id: if is_form {
                "form:plural"
            } else {
                "headword:common"
            }
            .to_owned(),
            source_kind: source_kind.to_owned(),
            source_node_id: is_form.then(Uuid::now_v7),
            content_scope: "draft".to_owned(),
            publication_id: None,
            surface: if is_form { "workspaces" } else { "workspace" }.to_owned(),
            normalized_surface: if is_form { "workspaces" } else { "workspace" }.to_owned(),
            dialect: "common".to_owned(),
            normalization_version: 1,
            source_revision: 1,
            event_offset: 1,
            pos_id: is_form.then(Uuid::now_v7),
            pos: is_form.then(|| "noun".to_owned()),
            form_type: is_form.then(|| "plural".to_owned()),
        }
    }

    #[test]
    fn suggested_forms_only_create_groups_for_parts_with_derived_forms() {
        let parts = [
            CatalogPartRecord {
                id: Uuid::now_v7(),
                code: "noun".to_owned(),
            },
            CatalogPartRecord {
                id: Uuid::now_v7(),
                code: "pronoun".to_owned(),
            },
        ];
        let forms = build_suggested_forms(
            &WordHeadwordsV2::Unified {
                common: "book".to_owned(),
            },
            &parts,
        );

        assert_eq!(forms.pos[0].form_groups.len(), 1);
        assert!(forms.pos[1].form_groups.is_empty());
    }

    #[test]
    fn detection_classifies_exact_cross_kind_and_explicit_form_surfaces_as_warnings() {
        let exact_candidate = headword_surface_candidates(
            &WordHeadwordsV2::Unified {
                common: "workspace".to_owned(),
            },
            EntryKind::Word,
        )
        .unwrap()
        .remove(0);
        let exact = surface_match(&exact_candidate, &source("headword", "word")).unwrap();
        assert_eq!(exact.match_category, SurfaceMatchCategoryV2::ExactHeadword);
        assert_eq!(exact.attention_level, SurfaceAttentionLevelV2::High);
        assert_eq!(exact.can_continue, SurfaceCanContinueTrue);

        let cross_kind = surface_match(&exact_candidate, &source("headword", "phrase")).unwrap();
        assert_eq!(
            cross_kind.match_category,
            SurfaceMatchCategoryV2::CrossKindHeadword
        );

        let form_candidate = headword_surface_candidates(
            &WordHeadwordsV2::Unified {
                common: "workspaces".to_owned(),
            },
            EntryKind::Word,
        )
        .unwrap()
        .remove(0);
        let form = surface_match(&form_candidate, &source("form", "word")).unwrap();
        assert_eq!(form.match_category, SurfaceMatchCategoryV2::HeadwordForm);
        assert_eq!(form.attention_level, SurfaceAttentionLevelV2::Normal);
        assert_eq!(form.can_continue, SurfaceCanContinueTrue);
        assert!(matches!(
            form.existing.source,
            ExistingSurfaceSourceV2::Form { .. }
        ));

        let mut base_source = source("form", "word");
        base_source.form_type = Some("base".to_owned());
        let base = surface_match(&exact_candidate, &base_source).unwrap();
        let ExistingSurfaceSourceV2::Form { form_type, .. } = base.existing.source else {
            panic!("base slot must remain a form source");
        };
        assert_eq!(form_type, WordFormTypeV2::Base);
        assert_eq!(serde_json::to_value(form_type).unwrap(), "base");
    }

    #[test]
    fn mixed_parity_falls_back_when_any_legacy_exact_entry_is_not_projected() {
        let candidate = headword_surface_candidates(
            &WordHeadwordsV2::Unified {
                common: "workspace".to_owned(),
            },
            EntryKind::Word,
        )
        .unwrap()
        .remove(0);
        let projected_source = source("headword", "word");
        let projected_id = projected_source.entry_id;
        let projected = surface_match(&candidate, &projected_source).unwrap();
        let legacy = vec![
            crate::lexicon::model::DuplicateRecord {
                entry_id: projected_id,
                headword: "workspace".to_owned(),
                dialect: "common".to_owned(),
                is_archived: false,
                is_published: false,
            },
            crate::lexicon::model::DuplicateRecord {
                entry_id: Uuid::now_v7(),
                headword: "workspace".to_owned(),
                dialect: "common".to_owned(),
                is_archived: false,
                is_published: true,
            },
        ];

        assert!(has_unprojected_legacy_exact(&legacy, &[projected]));
        let fully_projected = vec![legacy[0].clone()];
        let projected_again = surface_match(&candidate, &projected_source).unwrap();
        assert!(!has_unprojected_legacy_exact(
            &fully_projected,
            &[projected_again]
        ));

        let mut form_only_source = source("form", "word");
        form_only_source.entry_id = projected_id;
        let form_only = surface_match(&candidate, &form_only_source).unwrap();
        assert_eq!(form_only.existing.word_id, projected_id);
        assert_eq!(
            form_only.match_category,
            SurfaceMatchCategoryV2::HeadwordForm
        );
        assert!(has_unprojected_legacy_exact(&fully_projected, &[form_only]));
    }

    /// 每个元组是 (命中词条 id, 引用方词条 id, 关系类型)；关联词节点 id 自动生成，
    /// 同一元组重复出现即模拟「同一引用方在多个义项里写了同一种关系」。
    fn inbound(records: &[(Uuid, Uuid, &str)]) -> Vec<InboundRelationPreview> {
        let records = records
            .iter()
            .map(|(target, source, relation)| {
                inbound_record(*target, *source, relation, Uuid::now_v7())
            })
            .collect::<Vec<_>>();
        inbound_relation_previews(&records).unwrap()
    }

    fn inbound_record(
        target_entry_id: Uuid,
        source_entry_id: Uuid,
        relation: &str,
        source_node_id: Uuid,
    ) -> crate::lexicon::model::SurfaceInboundRelationRecord {
        crate::lexicon::model::SurfaceInboundRelationRecord {
            target_entry_id,
            source_entry_id,
            source_node_id,
            source_status: "published".to_owned(),
            source_headword_mode: "unified".to_owned(),
            source_dialect: None,
            source_common_headword: Some("apples".to_owned()),
            source_uk_headword: None,
            source_us_headword: None,
            draft_relation_type: Some(relation.to_owned()),
            source_snapshot: None,
        }
    }

    fn headword_candidate(surface: &str) -> HeadwordSurfaceCandidate {
        headword_surface_candidates(
            &WordHeadwordsV2::Unified {
                common: surface.to_owned(),
            },
            EntryKind::Word,
        )
        .unwrap()
        .remove(0)
    }

    #[test]
    fn relation_matches_annotate_the_entry_that_owns_the_surface() {
        let candidate = headword_candidate("workspace");
        let exact = surface_match(&candidate, &source("headword", "word")).unwrap();
        let target_id = exact.existing.word_id;
        let referencing_id = Uuid::now_v7();

        let derived = relation_surface_matches(
            std::slice::from_ref(&exact),
            &inbound(&[(target_id, referencing_id, "synonym")]),
        )
        .unwrap();

        assert_eq!(derived.len(), 1);
        let item = &derived[0];
        assert_eq!(
            item.match_category,
            SurfaceMatchCategoryV2::HeadwordRelation
        );
        assert_eq!(item.attention_level, SurfaceAttentionLevelV2::Normal);
        // 命中行归属拥有该词面的词条，引用方只是命中原因里的施动者。
        assert_eq!(item.existing.word_id, target_id);
        assert_ne!(item.match_id, exact.match_id);
        let ExistingSurfaceSourceV2::Relation {
            relation_type,
            referencing_word_id,
            referencing_headword,
            surface,
            ..
        } = &item.existing.source
        else {
            panic!("relation dimension must use the relation source variant");
        };
        assert_eq!(*relation_type, RelationTypeV2::Synonym);
        assert_eq!(*referencing_word_id, referencing_id);
        assert_eq!(referencing_headword, "apples");
        assert_eq!(surface, "workspace");
    }

    #[test]
    fn relation_matches_never_derive_from_form_hits() {
        let candidate = headword_candidate("workspaces");
        let form = surface_match(&candidate, &source("form", "word")).unwrap();

        let derived = relation_surface_matches(
            std::slice::from_ref(&form),
            &inbound(&[(form.existing.word_id, Uuid::now_v7(), "synonym")]),
        )
        .unwrap();

        assert!(derived.is_empty());
    }

    #[test]
    fn one_referencing_entry_and_relation_type_yields_one_row_across_scopes() {
        let candidate = headword_candidate("workspace");
        let mut draft_source = source("headword", "word");
        let mut published_source = draft_source.clone();
        published_source.content_scope = "current_publication".to_owned();
        published_source.publication_id = Some(Uuid::now_v7());
        published_source.source_id = "headword:common:published".to_owned();
        draft_source.entry_id = published_source.entry_id;
        let target_id = draft_source.entry_id;
        let referencing_id = Uuid::now_v7();
        let headword_matches = vec![
            surface_match(&candidate, &draft_source).unwrap(),
            surface_match(&candidate, &published_source).unwrap(),
        ];

        let derived = relation_surface_matches(
            &headword_matches,
            &inbound(&[
                (target_id, referencing_id, "synonym"),
                (target_id, referencing_id, "synonym"),
            ]),
        )
        .unwrap();

        assert_eq!(derived.len(), 1);
    }

    #[test]
    fn distinguish_candidates_share_one_relation_row_per_entry() {
        // uk 与 us 两个候选撞上同一个词条时，「被 X 引用为近义词」只说一次——
        // 主词命中每候选一行是因为两行的 surface 不同，关联词说明没有这个区别。
        let candidates = headword_surface_candidates(
            &WordHeadwordsV2::Distinguish {
                uk: "colour".to_owned(),
                us: "color".to_owned(),
                source_dialect: SourceDialect::Uk,
            },
            EntryKind::Word,
        )
        .unwrap();
        let mut uk_source = source("headword", "word");
        uk_source.surface = "colour".to_owned();
        uk_source.normalized_surface = "colour".to_owned();
        uk_source.dialect = "uk".to_owned();
        let mut us_source = uk_source.clone();
        us_source.surface = "color".to_owned();
        us_source.normalized_surface = "color".to_owned();
        us_source.dialect = "us".to_owned();
        us_source.source_id = "headword:us".to_owned();
        let target_id = uk_source.entry_id;
        let headword_matches = vec![
            surface_match(&candidates[0], &uk_source).unwrap(),
            surface_match(&candidates[1], &us_source).unwrap(),
        ];

        let derived = relation_surface_matches(
            &headword_matches,
            &inbound(&[(target_id, Uuid::now_v7(), "synonym")]),
        )
        .unwrap();

        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].existing.word_id, target_id);
    }

    #[test]
    fn every_distinct_referencing_entry_and_relation_type_gets_its_own_row() {
        let candidate = headword_candidate("workspace");
        let exact = surface_match(&candidate, &source("headword", "word")).unwrap();
        let target_id = exact.existing.word_id;
        // 命中条目由分页承载，关联词维度不再自设私有上限——否则命中行与
        // matched_entry_contexts 的 previews 会各自截出不同的子集。
        let inbound = inbound(
            &(0..8)
                .map(|_| (target_id, Uuid::now_v7(), "antonym"))
                .collect::<Vec<_>>(),
        );

        let derived = relation_surface_matches(std::slice::from_ref(&exact), &inbound).unwrap();

        assert_eq!(derived.len(), 8);
        let match_ids = derived
            .iter()
            .map(|item| item.match_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(match_ids.len(), 8, "每行必须有独立的 match_id");
    }

    #[test]
    fn relation_matches_do_not_change_the_surface_policy() {
        let candidate = headword_candidate("workspace");
        let cross_kind = surface_match(&candidate, &source("headword", "phrase")).unwrap();
        let derived = relation_surface_matches(
            std::slice::from_ref(&cross_kind),
            &inbound(&[(cross_kind.existing.word_id, Uuid::now_v7(), "derivative")]),
        )
        .unwrap();

        assert_eq!(derived.len(), 1);
        assert_eq!(
            surface_policy_name(&derived),
            SurfacePolicyNameV2::SurfaceWarningAcknowledgement
        );
    }

    #[test]
    fn surface_match_ids_changed_rejects_shrunk_and_expanded_sets() {
        assert!(!surface_match_ids_changed(["a", "b"], ["b", "a"]));
        assert!(surface_match_ids_changed(["a", "b"], ["a"]));
        assert!(surface_match_ids_changed(["a"], ["a", "b"]));
    }
}
