use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use super::*;
use crate::lexicon::dto::DraftMeaningsStepContent;
use crate::lexicon::dto::{
    AdminWordAny, AdminWordAnyEnvelope, AdminWordDraftAnyEnvelope, AdminWordDraftV3Envelope,
    AdminWordV3, AdminWordV3Capabilities, AdminWordV3Compatibility, BuiltinDictionaryEvidenceV3,
    CreateAdminWordV3Input, DetectLexiconSurfaceResponseV3,
    DetectLexiconSurfaceV3Input, DetectionSurfaceRequestEchoV3, DialectRulesV3,
    DictionaryCoverageV3, DictionaryProvenanceV3,
    DictionaryProviderEvidenceV3, DraftFormsStepContentV3, DraftMeaningsStepContentV3,
    DraftValidationResponseV3, EnglishLanguageV3, EntryPresentationV3, FormsImpactItemV3,
    FormsImpactNodeTypeV3, FormsImpactResponseV3, LegacyHeadwordsCompatibilityV3,
    PreviewFormsImpactInputV3, PronunciationNormalizationVersionV3, RetiredStableNodeV3,
    SaveFormsStepInputV3, SaveMeaningsStepInputV3, SuggestedCommonFormVariantV3,
    SuggestedConcreteFormV3, SuggestedRegionalVariantsV3, V3PublicationBlockCode,
    V3PublicationCapability, V3RetiredNodeRole, ValidateAdminWordV3Input, WordEntryKindV3,
    WordFormTypeV3, WordRegionalVariantsV3,
};
use crate::lexicon::model::NodeIdentityRecord;

const V3_CREATE_SCOPE: &str = "lexicon.entry.create.v3";
const V3_DETECTION_TTL: StdDuration = StdDuration::from_secs(5 * 60);
const V3_DETECTION_RETENTION_TTL: StdDuration = StdDuration::from_secs(65 * 60);

#[derive(Debug, sqlx::FromRow)]
struct V3EntryStateRecord {
    origin: String,
    publication_canary_enabled: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct V3PresentationRecord {
    label: String,
    matched_surfaces: Vec<String>,
    strategy_version: String,
}

#[derive(Debug, sqlx::FromRow)]
struct RetiredV3NodeRecord {
    id: Uuid,
    parent_node_id: Option<Uuid>,
    node_role: String,
    retired_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq)]
struct V3AuditNodeDelta {
    generated_node_ids: Vec<Uuid>,
    changed_node_ids: Vec<Uuid>,
    retired_node_ids: Vec<Uuid>,
}

impl LexiconService {
    pub async fn get_draft_any(
        &self,
        id: Uuid,
    ) -> Result<AdminWordDraftAnyEnvelope, LexiconServiceError> {
        let record = self
            .repository
            .entry_by_id(id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        if record.content_schema_version == 2 {
            let mut word = entry_from_record(record)?;
            self.hydrate_sentence_associations(&mut word).await?;
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
            return Ok(AdminWordDraftAnyEnvelope::V2(Box::new(
                AdminWordDraftV2Envelope {
                    word,
                    retired_stable_slots,
                },
            )));
        }
        if record.content_schema_version != 3 {
            return Err(LexiconServiceError::UnsupportedSchemaVersion(
                record.content_schema_version,
            ));
        }
        let word = self.entry_v3_from_record(record).await?;
        let retired_stable_nodes = self.retired_v3_nodes(id).await?;
        Ok(AdminWordDraftAnyEnvelope::V3(Box::new(
            AdminWordDraftV3Envelope {
                word,
                retired_stable_nodes,
            },
        )))
    }

    pub async fn get_v3(&self, id: Uuid) -> Result<AdminWordV3, LexiconServiceError> {
        let record = self
            .repository
            .entry_by_id(id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        self.entry_v3_from_record(record).await
    }

    async fn entry_v3_from_record(
        &self,
        record: EntryRecord,
    ) -> Result<AdminWordV3, LexiconServiceError> {
        if record.content_schema_version != 3 {
            return Err(LexiconServiceError::UnsupportedSchemaVersion(
                record.content_schema_version,
            ));
        }
        if record.language != "en" || record.kind != "word" {
            return Err(invariant_record());
        }
        let forms: DraftFormsStepContentV3 =
            serde_json::from_value(record.forms.clone()).map_err(serialization_error)?;
        let mut meanings: DraftMeaningsStepContentV3 =
            serde_json::from_value(record.meanings.clone()).map_err(serialization_error)?;
        self.hydrate_v3_sentence_associations(record.id, &mut meanings)
            .await?;
        let state = sqlx::query_as::<_, V3EntryStateRecord>(
            r#"
            SELECT origin, publication_canary_enabled
            FROM lexicon.v3_entry_state
            WHERE entry_id = $1
            "#,
        )
        .bind(record.id)
        .fetch_optional(self.repository.pool())
        .await
        .map_err(database_error)?
        .ok_or_else(invariant_record)?;
        let compatibility = if state.origin == "migrated_v2" {
            Some(AdminWordV3Compatibility {
                legacy_headwords: legacy_headwords_from_record(&record)?,
            })
        } else {
            None
        };
        let presentation = self
            .v3_presentation(record.id, record.revision, &forms, compatibility.as_ref())
            .await?;
        let completed_steps = record
            .completed_steps
            .iter()
            .filter_map(|step| match step.as_str() {
                "basics" => Some(PersistedWordStep::Basics),
                "forms" => Some(PersistedWordStep::Forms),
                "meanings" => Some(PersistedWordStep::Meanings),
                _ => None,
            })
            .collect();
        Ok(AdminWordV3 {
            schema_version: 3,
            id: record.id,
            language: EnglishLanguageV3::En,
            kind: WordEntryKindV3::Word,
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
            presentation,
            capabilities: AdminWordV3Capabilities {
                publication: match state.origin.as_str() {
                    "native" => V3PublicationCapability::Native,
                    "migrated_v2" => V3PublicationCapability::MigrationCanary {
                        whitelisted: state.publication_canary_enabled,
                        blocked_code: (!state.publication_canary_enabled)
                            .then_some(V3PublicationBlockCode::MigrationCanaryNotWhitelisted),
                    },
                    _ => return Err(invariant_record()),
                },
                pronunciation_normalization_version:
                    PronunciationNormalizationVersionV3::NfkcTrimLowerV1,
            },
            forms,
            meanings,
            compatibility,
            completed_steps,
            max_reachable_step: max_reachable_step(&record.completed_steps),
            created_by: record.created_by_admin_id,
            created_at: record.created_at,
            updated_at: record.updated_at,
            archived_at: record.archived_at,
            archived_by: record.archived_by_admin_id,
            published_at: record.current_published_at,
        })
    }

    async fn v3_presentation(
        &self,
        entry_id: Uuid,
        source_revision: i64,
        forms: &DraftFormsStepContentV3,
        compatibility: Option<&AdminWordV3Compatibility>,
    ) -> Result<EntryPresentationV3, LexiconServiceError> {
        let projected = sqlx::query_as::<_, V3PresentationRecord>(
            r#"
            SELECT label, matched_surfaces, strategy_version
            FROM lexicon.entry_presentation_projection
            WHERE entry_id = $1
              AND content_schema_version = 3
              AND source_revision = $2
            "#,
        )
        .bind(entry_id)
        .bind(source_revision)
        .fetch_optional(self.repository.pool())
        .await
        .map_err(database_error)?;
        if let Some(projected) = projected {
            return Ok(EntryPresentationV3 {
                label: projected.label,
                matched_surfaces: projected.matched_surfaces,
                strategy_version: projected.strategy_version,
            });
        }
        if let Some(compatibility) = compatibility {
            return Ok(
                crate::lexicon::v3_projection::presentation_from_legacy_bridge(
                    entry_id,
                    &compatibility.legacy_headwords,
                ),
            );
        }
        crate::lexicon::v3_projection::presentation_from_native_forms(entry_id, forms).map_err(
            |_| {
                LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
                    "validated V3 forms could not be presented",
                ))
            },
        )
    }

    async fn retired_v3_nodes(
        &self,
        entry_id: Uuid,
    ) -> Result<Vec<RetiredStableNodeV3>, LexiconServiceError> {
        let records = sqlx::query_as::<_, RetiredV3NodeRecord>(
            r#"
            SELECT id, parent_node_id, node_role,
                   removed_from_draft_at AS retired_at
            FROM lexicon.nodes
            WHERE entry_id = $1
              AND removed_from_draft_at IS NOT NULL
              AND node_role = ANY($2)
            ORDER BY removed_from_draft_at, id
            "#,
        )
        .bind(entry_id)
        .bind([
            "forms.pos",
            "forms.form_group",
            "forms.group_membership",
            "forms.concrete_form",
            "forms.form_variant:common",
            "forms.form_variant:uk",
            "forms.form_variant:us",
            "forms.pronunciation",
        ])
        .fetch_all(self.repository.pool())
        .await
        .map_err(database_error)?;
        records
            .into_iter()
            .map(|record| {
                Ok(RetiredStableNodeV3 {
                    id: record.id,
                    node_role: retired_role(&record.node_role)?,
                    parent_node_id: record.parent_node_id,
                    retired_at: record.retired_at,
                })
            })
            .collect()
    }

    pub async fn detect_v3(
        &self,
        actor_id: Uuid,
        input: DetectLexiconSurfaceV3Input,
    ) -> Result<DetectLexiconSurfaceResponseV3, LexiconServiceError> {
        let normalized = crate::lexicon::normalization::normalize_headword(&input.surface)
            .map_err(|_| LexiconServiceError::InvalidField {
                field: "surface",
                message: "surface must contain between 1 and 200 valid codepoints",
            })?;
        let term = self
            .repository
            .dictionary_term(&normalized.key)
            .await
            .map_err(repository_error)?;
        let builtin_dictionary = if let Some(term) = term {
            let suggested_pos = map_dictionary_pos(&term.pos);
            let suggested_forms = suggested_pos
                .iter()
                .map(|pos| SuggestedConcreteFormV3 {
                    pos: pos.clone(),
                    form_type: WordFormTypeV3::Base,
                    regional_variants: SuggestedRegionalVariantsV3::Common {
                        common: SuggestedCommonFormVariantV3 {
                            dialect: crate::lexicon::dto::CommonDialectV3::Common,
                            spelling: term.term.clone(),
                            pronunciations: Vec::new(),
                        },
                    },
                })
                .collect();
            let provider = DictionaryProviderEvidenceV3 {
                name: term.provider_name,
                version: term.provider_version,
            };
            BuiltinDictionaryEvidenceV3::Matched {
                provider: provider.clone(),
                suggested_pos,
                suggested_forms,
                coverage: DictionaryCoverageV3 {
                    forms: DictionaryCoverageStateV2::Partial,
                    pronunciations: DictionaryCoverageStateV2::Missing,
                    meanings: DictionaryCoverageStateV2::Missing,
                    examples: DictionaryCoverageStateV2::Missing,
                    frequency: DictionaryCoverageStateV2::Missing,
                },
                provenance: DictionaryProvenanceV3 {
                    forms: Some(provider),
                    pronunciations: None,
                    meanings: None,
                    examples: None,
                    frequency: None,
                },
            }
        } else {
            BuiltinDictionaryEvidenceV3::NotFound
        };
        let detection_id = Uuid::now_v7();
        let (matches, surface_match_page) = self
            .detect_v3_surface_warning(actor_id, detection_id, &normalized.key)
            .await?;
        let now = Utc::now();
        let detection = DetectLexiconSurfaceResponseV3 {
            schema_version: 3,
            detection_id,
            expires_at: now + Duration::from_std(V3_DETECTION_TTL).expect("five minutes is valid"),
            request: DetectionSurfaceRequestEchoV3 {
                language: input.language,
                kind: input.kind,
                surface: normalized.display,
            },
            normalized_surface: normalized.key,
            requires_acknowledgement: !matches.is_empty(),
            matches,
            surface_match_page,
            builtin_dictionary,
        };
        self.detections
            .save_v3(actor_id, &detection, V3_DETECTION_RETENTION_TTL)
            .await
            .map_err(LexiconServiceError::DetectionStore)?;
        Ok(detection)
    }

    pub async fn create_v3(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        idempotency_key: Uuid,
        input: CreateAdminWordV3Input,
        write_projection: bool,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        let request_hash = sha256_json(&input).map_err(serialization_error)?;
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{V3_CREATE_SCOPE}:{actor_id}:{idempotency_key}"))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) = LexiconRepository::idempotency(
            &mut transaction,
            V3_CREATE_SCOPE,
            actor_id,
            idempotency_key,
        )
        .await
        .map_err(repository_error)?
        {
            if existing.request_hash != request_hash {
                return Err(LexiconServiceError::IdempotencyConflict);
            }
            transaction.commit().await.map_err(database_error)?;
            return serde_json::from_value(existing.response_body).map_err(serialization_error);
        }
        let detection = self
            .detections
            .load_v3(actor_id, input.detection_id)
            .await
            .map_err(LexiconServiceError::DetectionStore)?
            .ok_or(LexiconServiceError::DetectionMismatch)?;
        if detection.expires_at <= Utc::now() {
            return Err(LexiconServiceError::DetectionExpired);
        }
        if detection.request.kind != input.kind {
            return Err(LexiconServiceError::DetectionMismatch);
        }
        let verified_surface = if write_projection {
            self.verify_v3_detection_surface_for_create(
                &mut transaction,
                actor_id,
                detection.detection_id,
                &detection.normalized_surface,
                input.confirmed_surface_match_token.as_deref(),
            )
            .await?
        } else {
            None
        };
        let entry_id = Uuid::now_v7();
        let now = Utc::now();
        let forms = DraftFormsStepContentV3::default();
        let meanings = DraftMeaningsStepContentV3::default();
        sqlx::query(
            r#"
            INSERT INTO lexicon.entries (
                id, content_schema_version, language, kind, revision,
                headword_mode, source_dialect, detection_snapshot,
                created_by_admin_id, updated_by_admin_id, created_at, updated_at
            ) VALUES ($1, 3, 'en', 'word', 1, NULL, NULL, $2, $3, $3, $4, $4)
            "#,
        )
        .bind(entry_id)
        .bind(serde_json::to_value(&detection).map_err(serialization_error)?)
        .bind(actor_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.v3_entry_state (
                entry_id, content_schema_version, origin, publication_canary_enabled
            ) VALUES ($1, 3, 'native', FALSE)
            "#,
        )
        .bind(entry_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_editor_projection (
                entry_id, forms, meanings, rebuilt_revision, updated_at
            ) VALUES ($1, $2, $3, 1, $4)
            "#,
        )
        .bind(entry_id)
        .bind(serde_json::to_value(&forms).map_err(serialization_error)?)
        .bind(serde_json::to_value(&meanings).map_err(serialization_error)?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_step_progress (
                entry_id, step, completed_revision, content_hash, completed_at
            ) VALUES ($1, 'basics', 1, $2, $3)
            "#,
        )
        .bind(entry_id)
        .bind(sha256_json(&detection).map_err(serialization_error)?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let presentation =
            crate::lexicon::v3_projection::presentation_from_native_forms(entry_id, &forms)
                .map_err(|_| invariant_record())?;
        if write_projection {
            upsert_presentation(&mut transaction, entry_id, 1, &presentation).await?;
        }
        let word = AdminWordV3 {
            schema_version: 3,
            id: entry_id,
            language: EnglishLanguageV3::En,
            kind: WordEntryKindV3::Word,
            status: AdminWordStatus::Draft,
            revision: 1,
            lifecycle_revision: 1,
            published_revision: None,
            has_unpublished_changes: false,
            presentation,
            capabilities: AdminWordV3Capabilities {
                publication: V3PublicationCapability::Native,
                pronunciation_normalization_version:
                    PronunciationNormalizationVersionV3::NfkcTrimLowerV1,
            },
            forms,
            meanings,
            compatibility: None,
            completed_steps: vec![PersistedWordStep::Basics],
            max_reachable_step: WordCreationStep::Forms,
            created_by: actor_id,
            created_at: now,
            updated_at: now,
            archived_at: None,
            archived_by: None,
            published_at: None,
        };
        let envelope = AdminWordAnyEnvelope {
            word: AdminWordAny::V3(Box::new(word)),
        };
        insert_v3_idempotency(
            &mut transaction,
            V3_CREATE_SCOPE,
            actor_id,
            idempotency_key,
            &request_hash,
            entry_id,
            201,
            serde_json::to_value(&envelope).map_err(serialization_error)?,
        )
        .await?;
        insert_v3_audit(
            &mut transaction,
            actor_id,
            request_id,
            "lexicon.entry.create.v3",
            entry_id,
            1,
            serde_json::json!({
                "schema_version": 3,
                "surface_snapshot_id": verified_surface.as_ref().map(|value| value.snapshot_id),
            }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        if let Some(confirmation) = &verified_surface
            && let Err(error) = self.surface_snapshots.remove_verified(confirmation).await
        {
            tracing::warn!(%error, snapshot_id = %confirmation.snapshot_id, "created V3 entry but failed to remove surface confirmation");
        }
        Ok(envelope)
    }

    pub async fn preview_forms_impact_v3(
        &self,
        actor_id: Uuid,
        entry_id: Uuid,
        mut input: PreviewFormsImpactInputV3,
        read_projection: bool,
    ) -> Result<FormsImpactResponseV3, LexiconServiceError> {
        canonicalize_v3_forms(&mut input.content)?;
        let current = self.get_v3(entry_id).await?;
        ensure_v3_active(&current)?;
        ensure_v3_revision(&current, input.base_revision)?;
        let issues =
            crate::lexicon::v3_contract::validate_forms(&input.content, StepSaveIntent::Save);
        if !issues.is_empty() {
            return Err(v3_validation_failed(issues));
        }
        let affected = forms_impact_v3(&current.forms, &input.content, &current.meanings)?;
        if read_projection
            && let Some(surface_match_page) = self
                .preview_v3_forms_surface_warning(
                    actor_id,
                    entry_id,
                    current.revision,
                    &input.content,
                    &affected,
                )
                .await?
        {
            return Ok(FormsImpactResponseV3 {
                schema_version: 3,
                base_revision: current.revision,
                requires_confirmation: !affected.is_empty(),
                affected,
                confirmation_token: None,
                surface_match_page: Some(surface_match_page),
            });
        }
        if affected.is_empty() {
            return Ok(FormsImpactResponseV3 {
                schema_version: 3,
                base_revision: current.revision,
                requires_confirmation: false,
                affected,
                confirmation_token: None,
                surface_match_page: None,
            });
        }
        let token = Uuid::now_v7();
        self.impacts
            .save(
                actor_id,
                token,
                &ImpactConfirmation {
                    entry_id,
                    base_revision: current.revision,
                    content_hash: sha256_json(&input.content).map_err(serialization_error)?,
                },
                IMPACT_TTL,
            )
            .await
            .map_err(LexiconServiceError::ImpactStore)?;
        Ok(FormsImpactResponseV3 {
            schema_version: 3,
            base_revision: current.revision,
            requires_confirmation: true,
            affected,
            confirmation_token: Some(token),
            surface_match_page: None,
        })
    }

    pub async fn save_forms_v3(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        mut input: SaveFormsStepInputV3,
        write_projection: bool,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        canonicalize_v3_forms(&mut input.content)?;
        let issues = crate::lexicon::v3_contract::validate_forms(&input.content, input.intent);
        if !issues.is_empty() {
            return Err(v3_validation_failed(issues));
        }
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        if write_projection {
            LexiconRepository::lock_surface_contexts(&mut transaction, &[entry_id])
                .await
                .map_err(repository_error)?;
        }
        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        ensure_v3_record_active(&record)?;
        if record.revision != input.base_revision {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        let current_forms: DraftFormsStepContentV3 =
            serde_json::from_value(record.forms.clone()).map_err(serialization_error)?;
        let current_form_pos_ids = current_forms
            .pos
            .iter()
            .map(|pos| pos.pos_id)
            .collect::<HashSet<_>>();
        let next_form_pos_ids = input
            .content
            .pos
            .iter()
            .map(|pos| pos.pos_id)
            .collect::<HashSet<_>>();
        let pos_ownership_unchanged = current_form_pos_ids == next_form_pos_ids;
        let mut meanings: DraftMeaningsStepContentV3 =
            serde_json::from_value(record.meanings).map_err(serialization_error)?;
        let forms_was_complete = record.completed_steps.iter().any(|step| step == "forms");
        let meanings_was_complete = record.completed_steps.iter().any(|step| step == "meanings");
        let affected = forms_impact_v3(&current_forms, &input.content, &meanings)?;
        if !write_projection && !affected.is_empty() {
            let Some(token) = input.confirmed_impact_token else {
                return Err(LexiconServiceError::DownstreamConfirmationRequired(
                    affected.iter().map(|item| item.node_id).collect(),
                ));
            };
            let confirmation = self
                .impacts
                .load(actor_id, token)
                .await
                .map_err(LexiconServiceError::ImpactStore)?;
            let expected_hash = sha256_json(&input.content).map_err(serialization_error)?;
            if confirmation.as_ref().is_none_or(|confirmation| {
                confirmation.entry_id != entry_id
                    || confirmation.base_revision != record.revision
                    || confirmation.content_hash != expected_hash
            }) {
                return Err(LexiconServiceError::DownstreamConfirmationRequired(
                    affected.iter().map(|item| item.node_id).collect(),
                ));
            }
        }
        let catalog_parts = resolve_v3_catalog_parts(&mut transaction, &input.content).await?;
        let next_revision = record.revision + 1;
        let surface_confirmation = if write_projection {
            Some(
                self.verify_v3_forms_surface_for_save(
                    &mut transaction,
                    actor_id,
                    entry_id,
                    record.revision,
                    next_revision,
                    &current_forms,
                    &input.content,
                    &affected,
                    input.confirmed_surface_match_token.as_deref(),
                    input.confirmed_impact_token,
                )
                .await?,
            )
        } else {
            None
        };
        reconcile_v3_meanings_after_forms(&mut meanings, &input.content);
        let aggregate_issues =
            crate::lexicon::v3_contract::validate_aggregate_node_limit(&input.content, &meanings);
        if !aggregate_issues.is_empty() {
            return Err(v3_validation_failed(aggregate_issues));
        }
        let relational_meanings: DraftMeaningsStepContent =
            serde_json::from_value(serde_json::to_value(&meanings).map_err(serialization_error)?)
                .map_err(serialization_error)?;
        let retained_sense_ids = relational_meanings
            .pos
            .iter()
            .flat_map(|pos| pos.senses.iter().map(|sense| sense.id))
            .collect::<Vec<_>>();
        if !LexiconRepository::current_inbound_sense_refs(
            &mut transaction,
            entry_id,
            &retained_sense_ids,
        )
        .await
        .map_err(repository_error)?
        .is_empty()
        {
            return Err(LexiconServiceError::FormReferenceConflict);
        }
        let audit_node_delta = preflight_v3_form_node_identities(
            &mut transaction,
            entry_id,
            &current_forms,
            &input.content,
        )
        .await?;
        let sub_parts = LexiconRepository::catalog_sub_parts_for_reference(&mut transaction)
            .await
            .map_err(repository_error)?
            .into_iter()
            .map(|part| (part.code, part.id))
            .collect::<HashMap<_, _>>();
        LexiconRepository::replace_meanings_content(
            &mut transaction,
            entry_id,
            &relational_meanings,
            &sub_parts,
        )
        .await
        .map_err(repository_error)?;
        replace_v3_forms(&mut transaction, entry_id, &input.content, &catalog_parts).await?;
        let now = Utc::now();
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entries
            SET revision = $2, updated_by_admin_id = $3, updated_at = $4
            WHERE id = $1 AND content_schema_version = 3 AND revision = $5
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .bind(actor_id)
        .bind(now)
        .bind(record.revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if updated.rows_affected() != 1 {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        sqlx::query(
            r#"
            UPDATE lexicon.entry_editor_projection
            SET forms = $2, meanings = $3, rebuilt_revision = $4, updated_at = $5
            WHERE entry_id = $1
            "#,
        )
        .bind(entry_id)
        .bind(serde_json::to_value(&input.content).map_err(serialization_error)?)
        .bind(serde_json::to_value(&meanings).map_err(serialization_error)?)
        .bind(next_revision)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let forms_complete = input.intent == StepSaveIntent::Complete
            || (forms_was_complete
                && crate::lexicon::v3_contract::validate_forms(
                    &input.content,
                    StepSaveIntent::Complete,
                )
                .is_empty());
        update_v3_step_progress(
            &mut transaction,
            entry_id,
            "forms",
            next_revision,
            &input.content,
            forms_complete,
        )
        .await?;
        let meaning_pos_ids = meanings
            .pos
            .iter()
            .map(|pos| pos.pos_id)
            .collect::<HashSet<_>>();
        update_v3_step_progress(
            &mut transaction,
            entry_id,
            "meanings",
            next_revision,
            &meanings,
            meanings_was_complete
                && forms_complete
                && pos_ownership_unchanged
                && next_form_pos_ids == meaning_pos_ids,
        )
        .await?;
        sqlx::query(
            r#"
            UPDATE lexicon.v3_entry_state
            SET first_v3_write_revision = COALESCE(first_v3_write_revision, $2)
            WHERE entry_id = $1
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let migration_batch_id = v3_migration_batch_id(&mut transaction, entry_id).await?;
        if write_projection {
            let presentation = crate::lexicon::v3_projection::presentation_from_native_forms(
                entry_id,
                &input.content,
            )
            .map_err(|_| invariant_record())?;
            upsert_presentation(&mut transaction, entry_id, next_revision, &presentation).await?;
            replace_v3_surface_projection(
                &mut transaction,
                entry_id,
                next_revision,
                &input.content,
            )
            .await?;
            if let Some(evidence) = surface_confirmation
                .as_ref()
                .and_then(|confirmation| confirmation.evidence.as_ref())
            {
                LexiconRepository::upsert_forms_surface_acknowledgement(&mut transaction, evidence)
                    .await
                    .map_err(repository_error)?;
            } else {
                LexiconRepository::delete_forms_surface_acknowledgement(&mut transaction, entry_id)
                    .await
                    .map_err(repository_error)?;
            }
        }
        insert_v3_audit(
            &mut transaction,
            actor_id,
            request_id,
            "lexicon.entry.forms.save.v3",
            entry_id,
            next_revision,
            v3_save_audit_metadata(input.intent, migration_batch_id, audit_node_delta),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        if let Some(confirmation) = surface_confirmation.as_ref().and_then(|confirmation| {
            confirmation
                .verified_surface
                .as_ref()
                .or(confirmation.verified_impact.as_ref())
        }) && let Err(error) = self.surface_snapshots.remove_verified(confirmation).await
        {
            tracing::warn!(%error, snapshot_id = %confirmation.snapshot_id, "saved V3 forms but failed to remove surface confirmation");
        }
        let word = self.get_v3(entry_id).await?;
        Ok(AdminWordAnyEnvelope {
            word: AdminWordAny::V3(Box::new(word)),
        })
    }

    pub async fn save_meanings_v3(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        input: SaveMeaningsStepInputV3,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        let SaveMeaningsStepInputV3 {
            base_revision,
            intent,
            content,
            ..
        } = input;
        let mut issues = crate::lexicon::v3_contract::validate_meanings(&content);
        if intent == StepSaveIntent::Complete {
            issues.extend(
                crate::lexicon::v3_contract::validate_complete_definition_grammar(&content),
            );
        }
        if !issues.is_empty() {
            return Err(v3_validation_failed(issues));
        }
        let mut relational_meanings: DraftMeaningsStepContent =
            serde_json::from_value(serde_json::to_value(content).map_err(serialization_error)?)
                .map_err(serialization_error)?;
        crate::lexicon::sentence_association::clear_sentence_associations(&mut relational_meanings);
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        LexiconRepository::lock_surface_contexts(&mut transaction, &[entry_id])
            .await
            .map_err(repository_error)?;
        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        ensure_v3_record_active(&record)?;
        if record.revision != base_revision {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        if intent == StepSaveIntent::Complete
            && !record.completed_steps.iter().any(|step| step == "forms")
        {
            return Err(LexiconServiceError::StepNotReachable);
        }
        let forms: DraftFormsStepContentV3 =
            serde_json::from_value(record.forms.clone()).map_err(serialization_error)?;
        let meanings_was_complete = record.completed_steps.iter().any(|step| step == "meanings");
        let current_relational_meanings: DraftMeaningsStepContent =
            serde_json::from_value(record.meanings.clone()).map_err(serialization_error)?;
        let form_pos = forms
            .pos
            .iter()
            .map(|pos| pos.pos_id)
            .collect::<HashSet<_>>();
        if relational_meanings
            .pos
            .iter()
            .any(|pos| !form_pos.contains(&pos.pos_id))
        {
            return Err(LexiconServiceError::UnprocessableField {
                field: "pos_id",
                message: "meanings must belong to a POS in the current V3 forms",
            });
        }
        let validation_forms = v3_meaning_validation_forms(&forms);
        let catalog = self
            .catalog_context_for_reference(&mut transaction, &validation_forms)
            .await?;
        let rich_text_is_safe = canonicalize_meanings(&mut relational_meanings);
        let mut affected_contexts = relation_target_entry_ids(&current_relational_meanings);
        affected_contexts.extend(relation_target_entry_ids(&relational_meanings));
        affected_contexts.sort_unstable();
        affected_contexts.dedup();
        LexiconRepository::lock_surface_contexts(&mut transaction, &affected_contexts)
            .await
            .map_err(repository_error)?;
        let (binding_issues, _) = self
            .resolve_pending_relation_targets(
                &mut transaction,
                actor_id,
                request_id,
                entry_id,
                &mut relational_meanings,
                PendingRelationResolution::BindExisting,
            )
            .await?;
        if !binding_issues.is_empty() {
            return Err(v3_validation_failed(binding_issues));
        }
        let reference_resolution = resolve_meaning_references(
            &mut transaction,
            entry_id,
            &mut relational_meanings,
            ReferenceResolutionMode::Canonicalize,
            false,
            &HashSet::new(),
        )
        .await?;
        if !reference_resolution.issues.is_empty() {
            return Err(v3_validation_failed(reference_resolution.issues));
        }
        let semantic_issues = validate_meanings(
            entry_id,
            &validation_forms,
            &relational_meanings,
            &v3_meaning_validation_headwords(),
            &catalog.sub_part_parents,
        );
        if !rich_text_is_safe
            || !meaning_storage_is_safe(
                entry_id,
                &validation_forms,
                &relational_meanings,
                &catalog.sub_part_parents,
            )
        {
            return Err(v3_validation_failed(meanings_storage_issues(
                entry_id,
                semantic_issues,
            )));
        }
        if intent == StepSaveIntent::Complete && !semantic_issues.is_empty() {
            return Err(v3_validation_failed(semantic_issues));
        }
        let canonical_content: DraftMeaningsStepContentV3 = serde_json::from_value(
            serde_json::to_value(&relational_meanings).map_err(serialization_error)?,
        )
        .map_err(serialization_error)?;
        let aggregate_issues =
            crate::lexicon::v3_contract::validate_aggregate_node_limit(&forms, &canonical_content);
        if !aggregate_issues.is_empty() {
            return Err(v3_validation_failed(aggregate_issues));
        }
        let proposed = proposed_nodes(&DraftFormsStepContent::default(), &relational_meanings);
        let proposed_ids = sorted_unique_node_ids(proposed.iter().map(|node| node.id));
        LexiconRepository::lock_node_ids(&mut transaction, &proposed_ids)
            .await
            .map_err(repository_error)?;
        let existing =
            LexiconRepository::node_identities(&mut transaction, entry_id, &proposed_ids)
                .await
                .map_err(repository_error)?;
        let node_issues =
            validate_node_identities(entry_id, &validation_forms, &proposed, &existing);
        if node_issues
            .iter()
            .any(|issue| issue.code == "stable_node_id_changed")
        {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }
        if !node_issues.is_empty() {
            return Err(v3_validation_failed(node_issues));
        }
        let audit_node_delta = v3_audit_node_delta(
            entry_id,
            &v3_meaning_node_ids(&current_relational_meanings),
            &proposed_ids,
            &existing,
        );
        LexiconRepository::replace_meanings_content(
            &mut transaction,
            entry_id,
            &relational_meanings,
            &catalog.sub_part_ids,
        )
        .await
        .map_err(repository_error)?;
        let next_revision = record.revision + 1;
        let now = Utc::now();
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entries
            SET revision = $2, updated_by_admin_id = $3, updated_at = $4
            WHERE id = $1 AND content_schema_version = 3 AND revision = $5
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .bind(actor_id)
        .bind(now)
        .bind(record.revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if updated.rows_affected() != 1 {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        sqlx::query(
            r#"
            UPDATE lexicon.entry_editor_projection
            SET meanings = $2, rebuilt_revision = $3, updated_at = $4
            WHERE entry_id = $1
            "#,
        )
        .bind(entry_id)
        .bind(serde_json::to_value(&canonical_content).map_err(serialization_error)?)
        .bind(next_revision)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let presentation_updated = sqlx::query(
            r#"
            UPDATE lexicon.entry_presentation_projection
            SET source_revision = $2, updated_at = $3
            WHERE entry_id = $1
              AND content_schema_version = 3
              AND source_revision = $4
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .bind(now)
        .bind(record.revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if presentation_updated.rows_affected() != 1 {
            return Err(invariant_record());
        }
        sqlx::query(
            r#"
            UPDATE lexicon.surface_sources
            SET source_revision = $2,
                updated_at = $3
            WHERE entry_id = $1
              AND content_schema_version = 3
              AND content_scope = 'draft'
              AND is_deleted = FALSE
              AND source_revision = $4
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .bind(now)
        .bind(record.revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        update_v3_step_progress(
            &mut transaction,
            entry_id,
            "meanings",
            next_revision,
            &canonical_content,
            semantic_issues.is_empty()
                && (intent == StepSaveIntent::Complete || meanings_was_complete),
        )
        .await?;
        sqlx::query(
            r#"
            UPDATE lexicon.v3_entry_state
            SET first_v3_write_revision = COALESCE(first_v3_write_revision, $2)
            WHERE entry_id = $1
            "#,
        )
        .bind(entry_id)
        .bind(next_revision)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let migration_batch_id = v3_migration_batch_id(&mut transaction, entry_id).await?;
        insert_v3_audit(
            &mut transaction,
            actor_id,
            request_id,
            "lexicon.entry.meanings.save.v3",
            entry_id,
            next_revision,
            v3_save_audit_metadata(intent, migration_batch_id, audit_node_delta),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        let word = self.get_v3(entry_id).await?;
        Ok(AdminWordAnyEnvelope {
            word: AdminWordAny::V3(Box::new(word)),
        })
    }

    pub async fn validate_v3(
        &self,
        entry_id: Uuid,
        input: ValidateAdminWordV3Input,
    ) -> Result<DraftValidationResponseV3, LexiconServiceError> {
        let word = self.get_v3(entry_id).await?;
        ensure_v3_active(&word)?;
        ensure_v3_revision(&word, input.base_revision)?;
        let mut issues =
            crate::lexicon::v3_contract::validate_forms(&word.forms, StepSaveIntent::Complete);
        issues.extend(crate::lexicon::v3_contract::validate_meanings(
            &word.meanings,
        ));
        issues.extend(
            crate::lexicon::v3_contract::validate_complete_definition_grammar(&word.meanings),
        );
        issues.extend(crate::lexicon::v3_contract::validate_aggregate_node_limit(
            &word.forms,
            &word.meanings,
        ));
        let validation_forms = v3_meaning_validation_forms(&word.forms);
        let mut relational_meanings: DraftMeaningsStepContent = serde_json::from_value(
            serde_json::to_value(&word.meanings).map_err(serialization_error)?,
        )
        .map_err(serialization_error)?;
        crate::lexicon::sentence_association::clear_sentence_associations(&mut relational_meanings);
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let catalog = self
            .catalog_context_for_reference(&mut transaction, &validation_forms)
            .await?;
        issues.extend(validate_meanings(
            entry_id,
            &validation_forms,
            &relational_meanings,
            &v3_meaning_validation_headwords(),
            &catalog.sub_part_parents,
        ));
        let reference_resolution = resolve_meaning_references(
            &mut transaction,
            entry_id,
            &mut relational_meanings,
            ReferenceResolutionMode::Verify,
            false,
            &HashSet::new(),
        )
        .await?;
        issues.extend(reference_resolution.issues);
        transaction.commit().await.map_err(database_error)?;
        Ok(DraftValidationResponseV3 {
            schema_version: 3,
            validated_revision: word.revision,
            valid: issues.is_empty(),
            issues: crate::lexicon::v3_contract::v3_issues(&issues),
        })
    }
}

fn legacy_headwords_from_record(
    record: &EntryRecord,
) -> Result<LegacyHeadwordsCompatibilityV3, LexiconServiceError> {
    match record.headword_mode.as_deref() {
        Some("unified") => Ok(LegacyHeadwordsCompatibilityV3::Unified {
            common: record
                .common_headword
                .clone()
                .ok_or_else(invariant_record)?,
        }),
        Some("distinguish") => Ok(LegacyHeadwordsCompatibilityV3::Distinguish {
            uk: record.uk_headword.clone().ok_or_else(invariant_record)?,
            us: record.us_headword.clone().ok_or_else(invariant_record)?,
            source_dialect: match record.source_dialect.as_deref() {
                Some("uk") => SourceDialect::Uk,
                Some("us") => SourceDialect::Us,
                _ => return Err(invariant_record()),
            },
        }),
        _ => Err(invariant_record()),
    }
}

fn retired_role(role: &str) -> Result<V3RetiredNodeRole, LexiconServiceError> {
    Ok(match role {
        "forms.pos" => V3RetiredNodeRole::Pos,
        "forms.form_group" => V3RetiredNodeRole::FormGroup,
        "forms.group_membership" => V3RetiredNodeRole::GroupMembership,
        "forms.concrete_form" => V3RetiredNodeRole::ConcreteForm,
        "forms.form_variant:common" => V3RetiredNodeRole::CommonVariant,
        "forms.form_variant:uk" => V3RetiredNodeRole::UkVariant,
        "forms.form_variant:us" => V3RetiredNodeRole::UsVariant,
        "forms.pronunciation" => V3RetiredNodeRole::Pronunciation,
        _ => return Err(invariant_record()),
    })
}

fn ensure_v3_active(word: &AdminWordV3) -> Result<(), LexiconServiceError> {
    if word.archived_at.is_some() {
        Err(LexiconServiceError::EntryArchived)
    } else {
        Ok(())
    }
}

fn ensure_v3_record_active(record: &EntryRecord) -> Result<(), LexiconServiceError> {
    if record.content_schema_version != 3 {
        return Err(LexiconServiceError::UnsupportedSchemaVersion(
            record.content_schema_version,
        ));
    }
    if record.archived_at.is_some() {
        return Err(LexiconServiceError::EntryArchived);
    }
    Ok(())
}

fn ensure_v3_revision(word: &AdminWordV3, revision: i64) -> Result<(), LexiconServiceError> {
    if word.revision == revision {
        Ok(())
    } else {
        Err(LexiconServiceError::RevisionConflict {
            current_revision: word.revision,
        })
    }
}

fn forms_impact_v3(
    current: &DraftFormsStepContentV3,
    proposed: &DraftFormsStepContentV3,
    current_meanings: &DraftMeaningsStepContentV3,
) -> Result<Vec<FormsImpactItemV3>, LexiconServiceError> {
    let proposed_ids = v3_form_node_types(proposed);
    let mut affected = v3_form_node_types(current)
        .into_iter()
        .filter_map(|(id, node_type)| {
            (!proposed_ids.contains_key(&id)).then_some(FormsImpactItemV3 {
                node_id: id,
                node_type,
                reason: "node_removed_from_draft".to_owned(),
            })
        })
        .collect::<Vec<_>>();
    let mut proposed_meanings = current_meanings.clone();
    reconcile_v3_meanings_after_forms(&mut proposed_meanings, proposed);
    let proposed_meaning_ids = v3_meaning_node_types(&proposed_meanings)?;
    affected.extend(
        v3_meaning_node_types(current_meanings)?
            .into_iter()
            .filter_map(|(id, node_type)| {
                (!proposed_meaning_ids.contains_key(&id)).then_some(FormsImpactItemV3 {
                    node_id: id,
                    node_type,
                    reason: "downstream_node_removed_with_pos".to_owned(),
                })
            }),
    );
    affected.sort_by_key(|item| item.node_id);
    Ok(affected)
}

fn reconcile_v3_meanings_after_forms(
    meanings: &mut DraftMeaningsStepContentV3,
    forms: &DraftFormsStepContentV3,
) {
    let active_pos = forms
        .pos
        .iter()
        .map(|pos| pos.pos_id)
        .collect::<HashSet<_>>();
    meanings.pos.retain(|pos| active_pos.contains(&pos.pos_id));
    meanings.pos.sort_by_key(|meaning_pos| {
        forms
            .pos
            .iter()
            .position(|form_pos| form_pos.pos_id == meaning_pos.pos_id)
            .unwrap_or(usize::MAX)
    });
}

fn v3_meaning_node_types(
    content: &DraftMeaningsStepContentV3,
) -> Result<HashMap<Uuid, FormsImpactNodeTypeV3>, LexiconServiceError> {
    let relational: DraftMeaningsStepContent =
        serde_json::from_value(serde_json::to_value(content).map_err(serialization_error)?)
            .map_err(serialization_error)?;
    Ok(
        proposed_nodes(&DraftFormsStepContent::default(), &relational)
            .into_iter()
            .filter(|node| node.step == PersistedWordStep::Meanings)
            .filter_map(|node| {
                let node_type = match node.node_type {
                    "grammar_structure" => FormsImpactNodeTypeV3::GrammarStructure,
                    "text_variant" => FormsImpactNodeTypeV3::TextVariant,
                    "sense" => FormsImpactNodeTypeV3::Sense,
                    "definition" => FormsImpactNodeTypeV3::Definition,
                    "sentence" => FormsImpactNodeTypeV3::Sentence,
                    "relation" => FormsImpactNodeTypeV3::Relation,
                    // sense groups are top-level and are retained by forms saves.
                    "sense_group" => return None,
                    _ => return None,
                };
                Some((node.id, node_type))
            })
            .collect(),
    )
}

fn v3_form_node_ids(content: &DraftFormsStepContentV3) -> Vec<Uuid> {
    let mut ids = v3_form_node_types(content).into_keys().collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

fn v3_meaning_node_ids(content: &DraftMeaningsStepContent) -> Vec<Uuid> {
    sorted_unique_node_ids(
        proposed_nodes(&DraftFormsStepContent::default(), content)
            .into_iter()
            .map(|node| node.id),
    )
}

fn sorted_unique_node_ids(ids: impl IntoIterator<Item = Uuid>) -> Vec<Uuid> {
    ids.into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn v3_form_proposed_nodes(content: &DraftFormsStepContentV3) -> Vec<ProposedNode> {
    let mut nodes = Vec::new();
    for pos in &content.pos {
        nodes.push(ProposedNode {
            id: pos.pos_id,
            node_type: "pos",
            step: PersistedWordStep::Forms,
            parent_node_id: None,
            node_role: "forms.pos".to_owned(),
            stable_slot: false,
        });
        for group in &pos.form_groups {
            nodes.push(ProposedNode {
                id: group.id,
                node_type: "form_group",
                step: PersistedWordStep::Forms,
                parent_node_id: Some(pos.pos_id),
                node_role: "forms.form_group".to_owned(),
                stable_slot: false,
            });
            for membership in &group.members {
                nodes.push(ProposedNode {
                    id: membership.id,
                    node_type: "group_membership",
                    step: PersistedWordStep::Forms,
                    parent_node_id: Some(group.id),
                    node_role: "forms.group_membership".to_owned(),
                    stable_slot: false,
                });
            }
        }
        for form in &pos.forms {
            nodes.push(ProposedNode {
                id: form.id,
                node_type: "concrete_form",
                step: PersistedWordStep::Forms,
                parent_node_id: Some(pos.pos_id),
                node_role: "forms.concrete_form".to_owned(),
                stable_slot: false,
            });
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => push_v3_form_variant_nodes(
                    &mut nodes,
                    form.id,
                    common.id,
                    "common",
                    common.pronunciations.iter().map(|value| value.id),
                ),
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    push_v3_form_variant_nodes(
                        &mut nodes,
                        form.id,
                        uk.id,
                        "uk",
                        uk.pronunciations.iter().map(|value| value.id),
                    );
                    push_v3_form_variant_nodes(
                        &mut nodes,
                        form.id,
                        us.id,
                        "us",
                        us.pronunciations.iter().map(|value| value.id),
                    );
                }
            }
        }
    }
    nodes
}

fn push_v3_form_variant_nodes(
    nodes: &mut Vec<ProposedNode>,
    form_id: Uuid,
    variant_id: Uuid,
    dialect: &str,
    pronunciation_ids: impl IntoIterator<Item = Uuid>,
) {
    nodes.push(ProposedNode {
        id: variant_id,
        node_type: "form_variant",
        step: PersistedWordStep::Forms,
        parent_node_id: Some(form_id),
        node_role: format!("forms.form_variant:{dialect}"),
        stable_slot: true,
    });
    nodes.extend(pronunciation_ids.into_iter().map(|id| ProposedNode {
        id,
        node_type: "pronunciation",
        step: PersistedWordStep::Forms,
        parent_node_id: Some(variant_id),
        node_role: "forms.pronunciation".to_owned(),
        stable_slot: false,
    }));
}

async fn preflight_v3_form_node_identities(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    current: &DraftFormsStepContentV3,
    proposed_content: &DraftFormsStepContentV3,
) -> Result<V3AuditNodeDelta, LexiconServiceError> {
    let proposed = v3_form_proposed_nodes(proposed_content);
    let proposed_ids = sorted_unique_node_ids(proposed.iter().map(|node| node.id));
    LexiconRepository::lock_node_ids(tx, &proposed_ids)
        .await
        .map_err(repository_error)?;
    let existing = LexiconRepository::node_identities(tx, entry_id, &proposed_ids)
        .await
        .map_err(repository_error)?;
    let mut locator_forms = v3_meaning_validation_forms(proposed_content);
    for (locator_pos, proposed_pos) in locator_forms.pos.iter_mut().zip(&proposed_content.pos) {
        locator_pos.form_groups = proposed_pos
            .form_groups
            .iter()
            .map(|group| WordFormGroupV2 {
                id: group.id,
                is_regular: group.is_regular,
                slots: Vec::new(),
            })
            .collect();
    }
    let node_issues = validate_node_identities(entry_id, &locator_forms, &proposed, &existing);
    if node_issues
        .iter()
        .any(|issue| issue.code == "stable_node_id_changed")
    {
        return Err(LexiconServiceError::StableNodeIdChanged);
    }
    if !node_issues.is_empty() {
        return Err(v3_validation_failed(node_issues));
    }
    Ok(v3_audit_node_delta(
        entry_id,
        &v3_form_node_ids(current),
        &proposed_ids,
        &existing,
    ))
}

fn v3_audit_node_delta(
    entry_id: Uuid,
    current_ids: &[Uuid],
    proposed_ids: &[Uuid],
    existing: &[NodeIdentityRecord],
) -> V3AuditNodeDelta {
    let current = current_ids.iter().copied().collect::<BTreeSet<_>>();
    let proposed = proposed_ids.iter().copied().collect::<BTreeSet<_>>();
    let persisted = existing
        .iter()
        .filter(|node| node.entry_id == entry_id)
        .map(|node| node.id)
        .collect::<BTreeSet<_>>();
    V3AuditNodeDelta {
        generated_node_ids: proposed.difference(&persisted).copied().collect(),
        changed_node_ids: proposed.intersection(&persisted).copied().collect(),
        retired_node_ids: current.difference(&proposed).copied().collect(),
    }
}

fn v3_save_audit_metadata(
    intent: StepSaveIntent,
    migration_batch_id: Option<Uuid>,
    delta: V3AuditNodeDelta,
) -> Value {
    serde_json::json!({
        "schema_version": 3,
        "migration_batch_id": migration_batch_id,
        "generated_node_ids": delta.generated_node_ids,
        "changed_node_ids": delta.changed_node_ids,
        "retired_node_ids": delta.retired_node_ids,
        "intent": intent,
    })
}

async fn v3_migration_batch_id(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<Option<Uuid>, LexiconServiceError> {
    sqlx::query_scalar("SELECT migration_batch_id FROM lexicon.v3_entry_state WHERE entry_id = $1")
        .bind(entry_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)
}

fn v3_form_node_types(content: &DraftFormsStepContentV3) -> HashMap<Uuid, FormsImpactNodeTypeV3> {
    let mut nodes = HashMap::new();
    for pos in &content.pos {
        nodes.insert(pos.pos_id, FormsImpactNodeTypeV3::Pos);
        for group in &pos.form_groups {
            nodes.insert(group.id, FormsImpactNodeTypeV3::FormGroup);
            for membership in &group.members {
                nodes.insert(membership.id, FormsImpactNodeTypeV3::Membership);
            }
        }
        for form in &pos.forms {
            nodes.insert(form.id, FormsImpactNodeTypeV3::Form);
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    nodes.insert(common.id, FormsImpactNodeTypeV3::Variant);
                    for pronunciation in &common.pronunciations {
                        nodes.insert(pronunciation.id, FormsImpactNodeTypeV3::Pronunciation);
                    }
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    nodes.insert(uk.id, FormsImpactNodeTypeV3::Variant);
                    for pronunciation in &uk.pronunciations {
                        nodes.insert(pronunciation.id, FormsImpactNodeTypeV3::Pronunciation);
                    }
                    nodes.insert(us.id, FormsImpactNodeTypeV3::Variant);
                    for pronunciation in &us.pronunciations {
                        nodes.insert(pronunciation.id, FormsImpactNodeTypeV3::Pronunciation);
                    }
                }
            }
        }
    }
    nodes
}

async fn resolve_v3_catalog_parts(
    tx: &mut Transaction<'_, Postgres>,
    content: &DraftFormsStepContentV3,
) -> Result<HashMap<String, Uuid>, LexiconServiceError> {
    let codes = content
        .pos
        .iter()
        .map(|pos| pos.pos.clone())
        .collect::<Vec<_>>();
    let parts = LexiconRepository::catalog_parts_for_reference(tx, &codes)
        .await
        .map_err(repository_error)?;
    let mapped = parts
        .into_iter()
        .map(|part| (part.code, part.id))
        .collect::<HashMap<_, _>>();
    if mapped.len() != codes.len() {
        return Err(LexiconServiceError::CatalogMismatch);
    }
    Ok(mapped)
}

async fn replace_v3_forms(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    content: &DraftFormsStepContentV3,
    catalog_parts: &HashMap<String, Uuid>,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        UPDATE lexicon.nodes
        SET removed_from_draft_at = now()
        WHERE entry_id = $1
          AND removed_from_draft_at IS NULL
          AND node_role = ANY($2)
        "#,
    )
    .bind(entry_id)
    .bind([
        "forms.pos",
        "forms.form_group",
        "forms.group_membership",
        "forms.concrete_form",
        "forms.form_variant:common",
        "forms.form_variant:uk",
        "forms.form_variant:us",
        "forms.pronunciation",
    ])
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    for statement in [
        "DELETE FROM lexicon.v3_pronunciations WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_form_variants WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_group_memberships WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_concrete_forms WHERE entry_id = $1",
        "DELETE FROM lexicon.v3_form_groups WHERE entry_id = $1",
    ] {
        sqlx::query(statement)
            .bind(entry_id)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
    }
    let active_pos = content.pos.iter().map(|pos| pos.pos_id).collect::<Vec<_>>();
    sqlx::query(
        r#"
        DELETE FROM lexicon.entry_pos
        WHERE entry_id = $1
          AND content_schema_version = 3
          AND NOT (id = ANY($2))
        "#,
    )
    .bind(entry_id)
    .bind(&active_pos)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;

    // Move every retained POS ordinal above the current range before assigning
    // the submitted 0..N order. The V3 ordinal index is immediate (not
    // deferrable), so a direct A/B swap would otherwise collide on the first
    // row update.
    sqlx::query(
        r#"
        WITH ordinal_bound AS (
            SELECT COALESCE(MAX(sort_order), 0) + 1 AS offset
            FROM lexicon.entry_pos
            WHERE entry_id = $1 AND content_schema_version = 3
        )
        UPDATE lexicon.entry_pos AS pos
        SET sort_order = pos.sort_order + ordinal_bound.offset
        FROM ordinal_bound
        WHERE pos.entry_id = $1 AND pos.content_schema_version = 3
        "#,
    )
    .bind(entry_id)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;

    for (pos_ordinal, pos) in content.pos.iter().enumerate() {
        upsert_v3_node(tx, pos.pos_id, entry_id, "pos", None, "forms.pos").await?;
        let part_id = catalog_parts
            .get(&pos.pos)
            .copied()
            .ok_or(LexiconServiceError::CatalogMismatch)?;
        let result = sqlx::query(
            r#"
            INSERT INTO lexicon.entry_pos (
                id, entry_id, part_of_speech_id, spelling_mode, phonetic_mode,
                sort_order, content_schema_version
            ) VALUES ($1, $2, $3, $4, $5, $6, 3)
            ON CONFLICT (id) DO UPDATE
            SET spelling_mode = EXCLUDED.spelling_mode,
                phonetic_mode = EXCLUDED.phonetic_mode,
                sort_order = EXCLUDED.sort_order
            WHERE lexicon.entry_pos.entry_id = EXCLUDED.entry_id
              AND lexicon.entry_pos.content_schema_version = 3
              AND lexicon.entry_pos.part_of_speech_id = EXCLUDED.part_of_speech_id
            "#,
        )
        .bind(pos.pos_id)
        .bind(entry_id)
        .bind(part_id)
        .bind(pos.dialect_rules.spelling_mode.as_str())
        .bind(pos.dialect_rules.phonetic_mode.as_str())
        .bind(pos_ordinal as i32)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
        if result.rows_affected() != 1 {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }

        for (group_ordinal, group) in pos.form_groups.iter().enumerate() {
            upsert_v3_node(
                tx,
                group.id,
                entry_id,
                "form_group",
                Some(pos.pos_id),
                "forms.form_group",
            )
            .await?;
            sqlx::query(
                r#"
                INSERT INTO lexicon.v3_form_groups (
                    id, entry_id, entry_pos_id, is_regular, ordinal
                ) VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(group.id)
            .bind(entry_id)
            .bind(pos.pos_id)
            .bind(group.is_regular)
            .bind(group_ordinal as i32)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
        }
        for (form_ordinal, form) in pos.forms.iter().enumerate() {
            upsert_v3_node(
                tx,
                form.id,
                entry_id,
                "concrete_form",
                Some(pos.pos_id),
                "forms.concrete_form",
            )
            .await?;
            sqlx::query(
                r#"
                INSERT INTO lexicon.v3_concrete_forms (
                    id, entry_id, entry_pos_id, form_type, ordinal
                ) VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(form.id)
            .bind(entry_id)
            .bind(pos.pos_id)
            .bind(v3_form_type_name(form.form_type))
            .bind(form_ordinal as i32)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
            match &form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    insert_v3_variant(
                        tx,
                        entry_id,
                        form.id,
                        "common",
                        common.id,
                        &common.spelling,
                        common.origin,
                        &common.pronunciations,
                    )
                    .await?;
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    insert_v3_variant(
                        tx,
                        entry_id,
                        form.id,
                        "uk",
                        uk.id,
                        &uk.spelling,
                        uk.origin,
                        &uk.pronunciations,
                    )
                    .await?;
                    insert_v3_variant(
                        tx,
                        entry_id,
                        form.id,
                        "us",
                        us.id,
                        &us.spelling,
                        us.origin,
                        &us.pronunciations,
                    )
                    .await?;
                }
            }
        }
        for group in &pos.form_groups {
            for (membership_ordinal, membership) in group.members.iter().enumerate() {
                upsert_v3_node(
                    tx,
                    membership.id,
                    entry_id,
                    "group_membership",
                    Some(group.id),
                    "forms.group_membership",
                )
                .await?;
                sqlx::query(
                    r#"
                    INSERT INTO lexicon.v3_group_memberships (
                        id, entry_id, entry_pos_id, form_group_id, form_id, ordinal
                    ) VALUES ($1, $2, $3, $4, $5, $6)
                    "#,
                )
                .bind(membership.id)
                .bind(entry_id)
                .bind(pos.pos_id)
                .bind(group.id)
                .bind(membership.form_id)
                .bind(membership_ordinal as i32)
                .execute(&mut **tx)
                .await
                .map_err(database_error)?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_v3_variant(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    form_id: Uuid,
    dialect: &str,
    variant_id: Uuid,
    spelling: &str,
    origin: TextOrigin,
    pronunciations: &[crate::lexicon::dto::WordPronunciationV3],
) -> Result<(), LexiconServiceError> {
    upsert_v3_node(
        tx,
        variant_id,
        entry_id,
        "form_variant",
        Some(form_id),
        &format!("forms.form_variant:{dialect}"),
    )
    .await?;
    // Draft variants are stable identity shells, so `intent=save` may persist
    // an empty spelling. Empty shells deliberately have no surface projection;
    // `intent=complete` rejects them before this storage path.
    let normalized_spelling = if spelling.is_empty() {
        String::new()
    } else {
        crate::lexicon::normalization::normalize_headword(spelling)
            .map_err(|_| invariant_record())?
            .key
    };
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_form_variants (
            id, entry_id, form_id, dialect, spelling, normalized_spelling,
            normalization_version, origin
        ) VALUES ($1, $2, $3, $4, $5, $6, 1, $7)
        "#,
    )
    .bind(variant_id)
    .bind(entry_id)
    .bind(form_id)
    .bind(dialect)
    .bind(spelling)
    .bind(normalized_spelling)
    .bind(text_origin_name(origin))
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    for (ordinal, pronunciation) in pronunciations.iter().enumerate() {
        upsert_v3_node(
            tx,
            pronunciation.id,
            entry_id,
            "pronunciation",
            Some(variant_id),
            "forms.pronunciation",
        )
        .await?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.v3_pronunciations (
                id, entry_id, form_variant_id, dict_phonetic, actual_pron,
                normalized_dict_phonetic, normalized_actual_pron, style,
                normalization_version, ordinal
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1, $9)
            "#,
        )
        .bind(pronunciation.id)
        .bind(entry_id)
        .bind(variant_id)
        .bind(&pronunciation.dict_phonetic)
        .bind(&pronunciation.actual_pron)
        .bind(normalize_v3_text(&pronunciation.dict_phonetic))
        .bind(normalize_v3_text(&pronunciation.actual_pron))
        .bind(pronunciation.style.map(pronunciation_style_name))
        .bind(ordinal as i32)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

async fn upsert_v3_node(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    entry_id: Uuid,
    node_type: &str,
    parent_node_id: Option<Uuid>,
    node_role: &str,
) -> Result<(), LexiconServiceError> {
    let stable_slot = matches!(
        node_role,
        "forms.form_variant:common" | "forms.form_variant:uk" | "forms.form_variant:us"
    );
    if stable_slot {
        let existing = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id
            FROM lexicon.nodes
            WHERE entry_id = $1
              AND parent_node_id = $2
              AND node_role = $3
              AND stable_slot = TRUE
            "#,
        )
        .bind(entry_id)
        .bind(parent_node_id)
        .bind(node_role)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
        if existing.is_some_and(|existing_id| existing_id != id) {
            return Err(LexiconServiceError::StableNodeIdChanged);
        }
    }
    let result = sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, parent_node_id, node_role, stable_slot
        ) VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (id) DO UPDATE
        SET removed_from_draft_at = NULL
        WHERE lexicon.nodes.entry_id = EXCLUDED.entry_id
          AND lexicon.nodes.node_type = EXCLUDED.node_type
          AND lexicon.nodes.parent_node_id IS NOT DISTINCT FROM EXCLUDED.parent_node_id
          AND lexicon.nodes.node_role = EXCLUDED.node_role
          AND lexicon.nodes.stable_slot = EXCLUDED.stable_slot
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(node_type)
    .bind(parent_node_id)
    .bind(node_role)
    .bind(stable_slot)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(LexiconServiceError::StableNodeIdChanged)
    }
}

async fn update_v3_step_progress<T: serde::Serialize>(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    step: &str,
    revision: i64,
    content: &T,
    complete: bool,
) -> Result<(), LexiconServiceError> {
    if !complete {
        sqlx::query("DELETE FROM lexicon.entry_step_progress WHERE entry_id = $1 AND step = $2")
            .bind(entry_id)
            .bind(step)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
        return Ok(());
    }
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_step_progress (
            entry_id, step, completed_revision, content_hash, completed_at
        ) VALUES ($1, $2, $3, $4, now())
        ON CONFLICT (entry_id, step) DO UPDATE
        SET completed_revision = EXCLUDED.completed_revision,
            content_hash = EXCLUDED.content_hash,
            completed_at = EXCLUDED.completed_at
        "#,
    )
    .bind(entry_id)
    .bind(step)
    .bind(revision)
    .bind(sha256_json(content).map_err(serialization_error)?)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

const fn v3_form_type_name(value: WordFormTypeV3) -> &'static str {
    match value {
        WordFormTypeV3::Base => "base",
        WordFormTypeV3::ThirdPersonSingular => "third_person_singular",
        WordFormTypeV3::PresentParticiple => "present_participle",
        WordFormTypeV3::PastTense => "past_tense",
        WordFormTypeV3::PastParticiple => "past_participle",
        WordFormTypeV3::Plural => "plural",
        WordFormTypeV3::Comparative => "comparative",
        WordFormTypeV3::Superlative => "superlative",
    }
}

const fn text_origin_name(value: TextOrigin) -> &'static str {
    match value {
        TextOrigin::Dictionary => "dictionary",
        TextOrigin::Converted => "converted",
        TextOrigin::Manual => "manual",
    }
}

const fn pronunciation_style_name(value: PronunciationStyle) -> &'static str {
    match value {
        PronunciationStyle::Normal => "normal",
        PronunciationStyle::Strong => "strong",
        PronunciationStyle::Weak => "weak",
    }
}

fn normalize_v3_text(value: &str) -> String {
    value.nfkc().collect::<String>().trim().to_lowercase()
}

fn canonicalize_v3_forms(content: &mut DraftFormsStepContentV3) -> Result<(), LexiconServiceError> {
    for pos in &mut content.pos {
        for form in &mut pos.forms {
            match &mut form.regional_variants {
                WordRegionalVariantsV3::Common { common } => {
                    canonicalize_v3_spelling(&mut common.spelling)?;
                }
                WordRegionalVariantsV3::UkUs { uk, us } => {
                    canonicalize_v3_spelling(&mut uk.spelling)?;
                    canonicalize_v3_spelling(&mut us.spelling)?;
                }
            }
        }
    }
    Ok(())
}

fn canonicalize_v3_spelling(spelling: &mut String) -> Result<(), LexiconServiceError> {
    if spelling.trim().is_empty() {
        spelling.clear();
        return Ok(());
    }
    let normalized = crate::lexicon::normalization::normalize_headword(spelling).map_err(|_| {
        LexiconServiceError::UnprocessableField {
            field: "spelling",
            message: "spelling must contain between 1 and 200 valid codepoints",
        }
    })?;
    *spelling = normalized.display;
    Ok(())
}

async fn replace_v3_surface_projection(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    revision: i64,
    forms: &DraftFormsStepContentV3,
) -> Result<(), LexiconServiceError> {
    let sources =
        crate::lexicon::v3_projection::form_variant_sources(entry_id, forms).map_err(|_| {
            LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
                "validated V3 forms could not be projected",
            ))
        })?;
    let event_offset = sqlx::query_scalar::<_, i64>(
        "SELECT nextval('lexicon.surface_projection_event_offset_seq')",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET source_revision = $2,
            event_offset = $3,
            is_deleted = TRUE,
            updated_at = now()
        WHERE entry_id = $1
          AND content_schema_version = 3
          AND content_scope = 'draft'
          AND (source_revision, event_offset) <= ($2, $3)
        "#,
    )
    .bind(entry_id)
    .bind(revision)
    .bind(event_offset)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    let source_count = sources.len();
    for source in sources {
        sqlx::query(
            r#"
            INSERT INTO lexicon.surface_sources (
                entry_id, source_id, source_kind, source_node_id,
                language, entry_kind, dialect, dialect_scope,
                surface, normalized_surface, normalization_version,
                source_revision, event_offset, is_deleted, content_scope, publication_id,
                pos_id, pos, form_type, content_schema_version,
                form_id, variant_id, group_ids, projection_version, updated_at
            ) VALUES (
                $1, $2, 'form_variant', $3,
                'en', 'word', $4, $5,
                $6, $7, $8,
                $9, $10, FALSE, 'draft', NULL,
                $11, $12, $13, 3,
                $14, $15, $16, $17, now()
            )
            ON CONFLICT (source_id, content_scope, dialect_scope, normalization_version)
            DO UPDATE SET
                entry_id = EXCLUDED.entry_id,
                source_kind = EXCLUDED.source_kind,
                source_node_id = EXCLUDED.source_node_id,
                language = EXCLUDED.language,
                entry_kind = EXCLUDED.entry_kind,
                dialect = EXCLUDED.dialect,
                surface = EXCLUDED.surface,
                normalized_surface = EXCLUDED.normalized_surface,
                source_revision = EXCLUDED.source_revision,
                event_offset = EXCLUDED.event_offset,
                is_deleted = FALSE,
                publication_id = NULL,
                pos_id = EXCLUDED.pos_id,
                pos = EXCLUDED.pos,
                form_type = EXCLUDED.form_type,
                content_schema_version = 3,
                form_id = EXCLUDED.form_id,
                variant_id = EXCLUDED.variant_id,
                group_ids = EXCLUDED.group_ids,
                projection_version = EXCLUDED.projection_version,
                updated_at = now()
            "#,
        )
        .bind(source.entry_id)
        .bind(source.source_id)
        .bind(source.variant_id)
        .bind(source.dialect.as_str())
        .bind(source.dialect_scope.as_str())
        .bind(source.surface)
        .bind(source.normalized_surface)
        .bind(source.normalization_version)
        .bind(revision)
        .bind(event_offset)
        .bind(source.pos_id)
        .bind(source.pos)
        .bind(v3_form_type_name(source.form_type))
        .bind(source.form_id)
        .bind(source.variant_id)
        .bind(source.group_ids)
        .bind(source.projection_version)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    sqlx::query(
        r#"
        INSERT INTO platform.outbox_events (
            id, aggregate_type, aggregate_id, aggregate_revision,
            event_type, payload, occurred_at, available_at
        ) VALUES (
            $1, 'lexicon.surface_projection', $2, $3,
            'lexicon.surface_projection_replaced', $4, now(), now()
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .bind(event_offset)
    .bind(serde_json::json!({
        "entry_id": entry_id,
        "content_schema_version": 3,
        "content_scope": "draft",
        "publication_id": Option::<Uuid>::None,
        "source_revision": revision,
        "event_offset": event_offset,
        "source_count": source_count,
    }))
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn upsert_presentation(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    revision: i64,
    presentation: &EntryPresentationV3,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_presentation_projection (
            entry_id, content_schema_version, label, matched_surfaces, strategy_version,
            source_revision, updated_at
        ) VALUES ($1, 3, $2, $3, $4, $5, now())
        ON CONFLICT (entry_id) DO UPDATE
        SET content_schema_version = EXCLUDED.content_schema_version,
            label = EXCLUDED.label,
            matched_surfaces = EXCLUDED.matched_surfaces,
            strategy_version = EXCLUDED.strategy_version,
            source_revision = EXCLUDED.source_revision,
            updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(entry_id)
    .bind(&presentation.label)
    .bind(&presentation.matched_surfaces)
    .bind(&presentation.strategy_version)
    .bind(revision)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

#[allow(clippy::too_many_arguments)]
async fn insert_v3_idempotency(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    actor_id: Uuid,
    idempotency_key: Uuid,
    request_hash: &[u8],
    resource_id: Uuid,
    response_status: i16,
    response_body: Value,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO platform.idempotency_records (
            scope, idempotency_key, actor_id, request_hash, resource_id,
            response_status, response_body, expires_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, now() + interval '24 hours')
        "#,
    )
    .bind(scope)
    .bind(idempotency_key)
    .bind(actor_id)
    .bind(request_hash)
    .bind(resource_id)
    .bind(response_status)
    .bind(response_body)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

#[allow(clippy::too_many_arguments)]
async fn insert_v3_audit(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    request_id: Uuid,
    action: &str,
    resource_id: Uuid,
    revision: i64,
    metadata: Value,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO audit.admin_actions (
            id, actor_admin_id, action, resource_type, resource_id,
            resource_revision, request_id, metadata
        )
        SELECT $1, $2, $3, 'lexicon.entry', $4, $5, $6, $7
        WHERE NOT EXISTS (
            SELECT 1
            FROM audit.admin_actions
            WHERE actor_admin_id = $2
              AND action = $3
              AND resource_type = 'lexicon.entry'
              AND resource_id = $4
              AND request_id = $6
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(actor_id)
    .bind(action)
    .bind(resource_id)
    .bind(revision)
    .bind(request_id)
    .bind(metadata)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::PgPool;
    use tokio::sync::oneshot;

    use super::*;
    use crate::lexicon::dto::{
        CommonDialectV3, DialectRulesV3, V3ValidationIssueCode, WordCommonFormVariantV3,
        WordConcreteFormV3, WordFormGroupMemberV3, WordFormGroupV3, WordPosFormsV3,
        WordPronunciationV3,
    };

    fn fixed_id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn common_form(form_id: Uuid, variant_id: Uuid, pronunciation_id: Uuid) -> WordConcreteFormV3 {
        WordConcreteFormV3 {
            id: form_id,
            form_type: WordFormTypeV3::Base,
            regional_variants: WordRegionalVariantsV3::Common {
                common: WordCommonFormVariantV3 {
                    id: variant_id,
                    dialect: CommonDialectV3::Common,
                    spelling: format!("form-{form_id}"),
                    origin: TextOrigin::Manual,
                    pronunciations: vec![WordPronunciationV3 {
                        id: pronunciation_id,
                        dict_phonetic: "test".to_owned(),
                        actual_pron: "test".to_owned(),
                        style: Some(PronunciationStyle::Normal),
                    }],
                },
            },
        }
    }

    fn two_form_content(reverse: bool) -> DraftFormsStepContentV3 {
        let first_form = common_form(fixed_id(400), fixed_id(401), fixed_id(402));
        let second_form = common_form(fixed_id(500), fixed_id(501), fixed_id(502));
        let first_group = WordFormGroupV3 {
            id: fixed_id(200),
            is_regular: true,
            members: vec![WordFormGroupMemberV3 {
                id: fixed_id(201),
                form_id: first_form.id,
            }],
        };
        let second_group = WordFormGroupV3 {
            id: fixed_id(300),
            is_regular: false,
            members: vec![WordFormGroupMemberV3 {
                id: fixed_id(301),
                form_id: second_form.id,
            }],
        };
        let (forms, form_groups) = if reverse {
            (
                vec![second_form, first_form],
                vec![second_group, first_group],
            )
        } else {
            (
                vec![first_form, second_form],
                vec![first_group, second_group],
            )
        };
        DraftFormsStepContentV3 {
            pos: vec![WordPosFormsV3 {
                pos_id: fixed_id(100),
                pos: "noun".to_owned(),
                dialect_rules: DialectRulesV3::UNIFIED,
                forms,
                form_groups,
            }],
        }
    }

    async fn seed_admin(pool: &PgPool) -> Uuid {
        let admin_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'V3 node test')",
        )
        .bind(admin_id)
        .bind(format!("v3-node-{}", admin_id.simple()))
        .execute(pool)
        .await
        .unwrap();
        admin_id
    }

    async fn seed_v3_entry(
        pool: &PgPool,
        admin_id: Uuid,
        migration_batch_id: Option<Uuid>,
    ) -> Uuid {
        let entry_id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO lexicon.entries (
                id, content_schema_version, language, kind, revision,
                headword_mode, source_dialect, detection_snapshot,
                created_by_admin_id, updated_by_admin_id
            ) VALUES ($1, 3, 'en', 'word', 1, NULL, NULL, '{}', $2, $2)
            "#,
        )
        .bind(entry_id)
        .bind(admin_id)
        .execute(pool)
        .await
        .unwrap();
        if let Some(batch_id) = migration_batch_id {
            sqlx::query(
                r#"
                INSERT INTO lexicon.v3_entry_state (
                    entry_id, origin, migration_batch_id, source_revision
                ) VALUES ($1, 'migrated_v2', $2, 1)
                "#,
            )
            .bind(entry_id)
            .bind(batch_id)
            .execute(pool)
            .await
            .unwrap();
        } else {
            sqlx::query(
                "INSERT INTO lexicon.v3_entry_state (entry_id, origin) VALUES ($1, 'native')",
            )
            .bind(entry_id)
            .execute(pool)
            .await
            .unwrap();
        }
        entry_id
    }

    #[test]
    fn audit_delta_is_sorted_and_separates_new_retained_and_retired_ids() {
        let entry_id = fixed_id(1);
        let other_entry_id = fixed_id(2);
        let current = vec![fixed_id(30), fixed_id(10), fixed_id(20)];
        let proposed = vec![fixed_id(40), fixed_id(30), fixed_id(20)];
        let existing = vec![
            NodeIdentityRecord {
                id: fixed_id(30),
                entry_id,
                node_type: "concrete_form".to_owned(),
                parent_node_id: Some(fixed_id(100)),
                node_role: "forms.concrete_form".to_owned(),
                stable_slot: false,
            },
            NodeIdentityRecord {
                id: fixed_id(20),
                entry_id,
                node_type: "form_group".to_owned(),
                parent_node_id: Some(fixed_id(100)),
                node_role: "forms.form_group".to_owned(),
                stable_slot: false,
            },
            NodeIdentityRecord {
                id: fixed_id(40),
                entry_id: other_entry_id,
                node_type: "concrete_form".to_owned(),
                parent_node_id: Some(fixed_id(101)),
                node_role: "forms.concrete_form".to_owned(),
                stable_slot: false,
            },
        ];

        let delta = v3_audit_node_delta(entry_id, &current, &proposed, &existing);

        assert_eq!(delta.generated_node_ids, vec![fixed_id(40)]);
        assert_eq!(delta.changed_node_ids, vec![fixed_id(20), fixed_id(30)]);
        assert_eq!(delta.retired_node_ids, vec![fixed_id(10)]);
        let metadata = v3_save_audit_metadata(StepSaveIntent::Complete, Some(fixed_id(9)), delta);
        assert_eq!(metadata["schema_version"], 3);
        assert_eq!(metadata["migration_batch_id"], fixed_id(9).to_string());
        assert_eq!(metadata["intent"], "complete");
    }

    #[sqlx::test]
    async fn retired_v3_variant_slot_rejects_a_replacement_uuid_before_writes(pool: PgPool) {
        let admin_id = seed_admin(&pool).await;
        let entry_id = seed_v3_entry(&pool, admin_id, None).await;
        let proposed_content = two_form_content(false);
        let proposed = v3_form_proposed_nodes(&proposed_content);
        let mut tx = pool.begin().await.unwrap();
        for node in proposed
            .iter()
            .filter(|node| node.node_type != "form_variant" && node.node_type != "pronunciation")
        {
            upsert_v3_node(
                &mut tx,
                node.id,
                entry_id,
                node.node_type,
                node.parent_node_id,
                &node.node_role,
            )
            .await
            .unwrap();
        }
        let retired_variant_id = fixed_id(999);
        upsert_v3_node(
            &mut tx,
            retired_variant_id,
            entry_id,
            "form_variant",
            Some(fixed_id(400)),
            "forms.form_variant:common",
        )
        .await
        .unwrap();
        sqlx::query("UPDATE lexicon.nodes SET removed_from_draft_at = now() WHERE id = $1")
            .bind(retired_variant_id)
            .execute(&mut *tx)
            .await
            .unwrap();

        let error = preflight_v3_form_node_identities(
            &mut tx,
            entry_id,
            &DraftFormsStepContentV3::default(),
            &proposed_content,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, LexiconServiceError::StableNodeIdChanged));
        let proposed_variant_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lexicon.nodes WHERE id = $1)")
                .bind(fixed_id(401))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert!(!proposed_variant_exists, "preflight 冲突不得先写入新节点");
        tx.rollback().await.unwrap();
    }

    #[sqlx::test]
    async fn reverse_order_cross_entry_node_reuse_waits_then_returns_stable_validation(
        pool: PgPool,
    ) {
        let admin_id = seed_admin(&pool).await;
        let first_entry_id = seed_v3_entry(&pool, admin_id, None).await;
        let second_entry_id = seed_v3_entry(&pool, admin_id, None).await;
        let first_content = two_form_content(false);
        let second_content = two_form_content(true);
        let mut first_tx = pool.begin().await.unwrap();
        preflight_v3_form_node_identities(
            &mut first_tx,
            first_entry_id,
            &DraftFormsStepContentV3::default(),
            &first_content,
        )
        .await
        .unwrap();

        let second_pool = pool.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let mut second = tokio::spawn(async move {
            let mut tx = second_pool.begin().await.unwrap();
            started_tx.send(()).unwrap();
            let result = preflight_v3_form_node_identities(
                &mut tx,
                second_entry_id,
                &DraftFormsStepContentV3::default(),
                &second_content,
            )
            .await;
            tx.rollback().await.unwrap();
            result
        });
        started_rx.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "第二个逆序提交应等待第一个事务持有的排序 advisory locks"
        );

        for node in v3_form_proposed_nodes(&first_content) {
            upsert_v3_node(
                &mut first_tx,
                node.id,
                first_entry_id,
                node.node_type,
                node.parent_node_id,
                &node.node_role,
            )
            .await
            .unwrap();
        }
        first_tx.commit().await.unwrap();

        let error = tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .expect("排序锁释放后不得死锁")
            .unwrap()
            .unwrap_err();
        let LexiconServiceError::ValidationFailedV3(issues) = error else {
            panic!("跨词条 UUID 应返回稳定 V3 validation，而不是数据库错误: {error:?}");
        };
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == V3ValidationIssueCode::NodeIdReused)
        );
        let second_nodes: i64 =
            sqlx::query_scalar("SELECT count(*) FROM lexicon.nodes WHERE entry_id = $1")
                .bind(second_entry_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(second_nodes, 0, "冲突事务不得留下部分节点");
    }

    #[sqlx::test]
    async fn save_audit_includes_migration_batch_and_retry_is_action_idempotent(pool: PgPool) {
        let admin_id = seed_admin(&pool).await;
        let batch_id = Uuid::now_v7();
        let entry_id = seed_v3_entry(&pool, admin_id, Some(batch_id)).await;
        let request_id = Uuid::now_v7();
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(
            v3_migration_batch_id(&mut tx, entry_id).await.unwrap(),
            Some(batch_id)
        );
        let metadata = v3_save_audit_metadata(
            StepSaveIntent::Save,
            Some(batch_id),
            V3AuditNodeDelta {
                generated_node_ids: vec![fixed_id(10)],
                changed_node_ids: vec![fixed_id(20)],
                retired_node_ids: vec![fixed_id(30)],
            },
        );
        for action in [
            "lexicon.entry.forms.save.v3",
            "lexicon.entry.meanings.save.v3",
        ] {
            insert_v3_audit(
                &mut tx,
                admin_id,
                request_id,
                action,
                entry_id,
                2,
                metadata.clone(),
            )
            .await
            .unwrap();
            insert_v3_audit(
                &mut tx,
                admin_id,
                request_id,
                action,
                entry_id,
                2,
                metadata.clone(),
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();

        let rows: Vec<(String, Value)> = sqlx::query_as(
            r#"
            SELECT action, metadata
            FROM audit.admin_actions
            WHERE resource_id = $1 AND request_id = $2
            ORDER BY action
            "#,
        )
        .bind(entry_id)
        .bind(request_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2, "同一 action 的重试不得重复审计");
        assert_eq!(rows[0].1["migration_batch_id"], batch_id.to_string());
        assert_eq!(rows[0].1["generated_node_ids"][0], fixed_id(10).to_string());
        assert_eq!(rows[0].1["changed_node_ids"][0], fixed_id(20).to_string());
        assert_eq!(rows[0].1["retired_node_ids"][0], fixed_id(30).to_string());
    }
}
