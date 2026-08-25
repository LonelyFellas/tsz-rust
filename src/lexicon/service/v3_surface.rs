use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::DateTime;
use serde::Serialize;
use serde_json::Value;
use sqlx::{Postgres, Transaction};

use super::*;
use crate::lexicon::{
    dto::{
        Dialect, DraftMeaningsStepContentV3, EntryKind, ExistingSurfaceMatchV2,
        ExistingSurfaceSourceV2, FormSurfaceMatchV3, FormsImpactItemV3, LegacySurfaceMatchV3,
        MatchedEntryContextV3, RelationReferenceCountsV2, RelationReferencePreviewV2,
        RelationReferencePreviewV3, RelationReferenceSummaryV2, RelationReferenceSummaryV3,
        SurfaceAttentionLevelV2, SurfaceCanContinueTrue, SurfaceConfirmationReasonV2,
        SurfaceContentScopeV2, SurfaceMatchCandidateV2, SurfaceMatchCategoryV2, SurfaceMatchItemV3,
        SurfaceMatchPageV3, SurfaceMatchSeverityV2, WordDefinitionV3, WordFormTypeV2,
        WordFormTypeV3, WordSenseV3,
    },
    repository::SurfaceLockKey,
    surface_snapshot::{
        V3_SURFACE_PAGE_DATA_KEY, V3SurfaceSnapshotItem, V3SurfaceSnapshotPageData, surface_page_v3,
    },
    v3_projection::V3FormVariantSurfaceSource,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct V3SurfaceQueryKey {
    dialect_scope: String,
    normalized_surface: String,
}

#[derive(Debug, sqlx::FromRow)]
struct V3SurfaceSourceRecord {
    content_schema_version: i16,
    source_kind: String,
    source_id: String,
    source_node_id: Option<Uuid>,
    dialect_scope: String,
    normalized_surface: String,
    entry_id: Uuid,
    entry_headword: String,
    entry_kind: String,
    lifecycle_status: String,
    content_scope: String,
    publication_id: Option<Uuid>,
    pos_id: Option<Uuid>,
    pos: Option<String>,
    group_ids: Option<Vec<Uuid>>,
    form_id: Option<Uuid>,
    variant_id: Option<Uuid>,
    form_type: Option<String>,
    dialect: String,
    surface: String,
}

#[derive(Debug, sqlx::FromRow)]
struct V3SurfaceContextRecord {
    entry_id: Uuid,
    content_schema_version: i16,
    forms: Value,
    meanings: Value,
    label: String,
    matched_surfaces: Vec<String>,
    strategy_version: String,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct V3RelationSourcePresentationRecord {
    entry_id: Uuid,
    label: String,
    matched_surfaces: Vec<String>,
    strategy_version: String,
}

#[derive(Debug, sqlx::FromRow)]
struct V3RestorePublicSurfaceRecord {
    content_schema_version: i16,
    lifecycle_status: String,
    publication_id: Option<Uuid>,
    pos_id: Option<Uuid>,
    group_ids: Option<Vec<Uuid>>,
    form_id: Option<Uuid>,
    variant_id: Option<Uuid>,
    form_type: Option<String>,
    dialect: String,
    surface: String,
}

#[derive(Debug, Clone)]
struct ResolvedV3SurfaceMatch {
    match_id: String,
    item: SurfaceMatchItemV3,
    pos: Option<String>,
    matched_keys: BTreeSet<V3SurfaceQueryKey>,
}

#[derive(Debug)]
struct V3SurfaceMaterial {
    matches: Vec<ResolvedV3SurfaceMatch>,
    contexts: Vec<MatchedEntryContextV3>,
}

#[derive(Debug, Default)]
pub(super) struct V3RestoreSurfaceContribution {
    pub(super) items: Vec<LexiconSurfaceMatchV2>,
    pub(super) page_items: Vec<V3SurfaceSnapshotItem>,
    pub(super) candidate_evidence: Vec<Value>,
}

#[derive(Debug, Default)]
pub(super) struct V2RestorePublicationSurfaceContribution {
    pub(super) items: Vec<LexiconSurfaceMatchV2>,
    pub(super) contexts: Vec<MatchedEntryContextV2>,
}

#[derive(Debug)]
struct V2RestorePublicationCandidate {
    entry_id: Uuid,
    source_id: String,
    source_kind: &'static str,
    source_node_id: Option<Uuid>,
    entry_kind: EntryKind,
    dialect: Dialect,
    surface: String,
    normalized_surface: String,
    pos_id: Option<Uuid>,
    pos: Option<String>,
    form_type: Option<WordFormTypeV2>,
    lookup_keys: BTreeSet<V3SurfaceQueryKey>,
}

#[derive(Debug, Default)]
struct V3RelationSummaryBuilder {
    synonym: u32,
    antonym: u32,
    derivative: u32,
    total: u32,
    previews: Vec<RelationReferencePreviewV3>,
}

pub(super) fn v3_restore_synthetic_contexts(
    data: &V3SurfaceSnapshotPageData,
) -> Vec<MatchedEntryContextV2> {
    data.matched_entry_contexts
        .iter()
        .map(v3_context_to_v2)
        .collect()
}

impl V3SurfaceMaterial {
    fn public_matches(&self) -> Vec<SurfaceMatchItemV3> {
        self.matches.iter().map(|item| item.item.clone()).collect()
    }

    fn match_ids(&self) -> Vec<String> {
        self.matches
            .iter()
            .map(|item| item.match_id.clone())
            .collect()
    }

    fn page_data(&self) -> V3SurfaceSnapshotPageData {
        V3SurfaceSnapshotPageData {
            items: self
                .matches
                .iter()
                .map(|item| V3SurfaceSnapshotItem {
                    match_id: item.match_id.clone(),
                    item: item.item.clone(),
                })
                .collect(),
            matched_entry_contexts: self.contexts.clone(),
        }
    }

    fn synthetic_items(&self) -> Result<Vec<LexiconSurfaceMatchV2>, LexiconServiceError> {
        let presentations = self
            .contexts
            .iter()
            .map(|context| (context.entry_id, context.presentation.label.as_str()))
            .collect::<HashMap<_, _>>();
        self.matches
            .iter()
            .map(|resolved| {
                let (spelling, dialect, existing) = match &resolved.item {
                    SurfaceMatchItemV3::LegacyV2(item) => {
                        let (spelling, dialect) = legacy_surface_and_dialect(&item.existing.source);
                        (spelling, dialect, item.existing.clone())
                    }
                    SurfaceMatchItemV3::FormVariantV3(item) => {
                        let presentation = presentations
                            .get(&item.entry_id)
                            .ok_or_else(invariant_record)?;
                        (
                            item.spelling.as_str(),
                            item.dialect,
                            ExistingSurfaceMatchV2 {
                                word_id: item.entry_id,
                                headword: (*presentation).to_owned(),
                                kind: EntryKind::Word,
                                status: item.status,
                                source: ExistingSurfaceSourceV2::Form {
                                    source_id: format!("v3:form_variant:{}", item.variant_id),
                                    source_node_id: item.variant_id,
                                    content_scope: item.content_scope,
                                    surface: item.spelling.clone(),
                                    dialect: item.dialect,
                                    pos_id: item.pos_id,
                                    pos: resolved.pos.clone().ok_or_else(invariant_record)?,
                                    form_type: v2_form_type(item.form_type),
                                },
                            },
                        )
                    }
                };
                let normalized = normalize_headword(spelling).map_err(|_| invariant_record())?;
                Ok(LexiconSurfaceMatchV2 {
                    match_id: resolved.match_id.clone(),
                    match_category: SurfaceMatchCategoryV2::FormForm,
                    severity: SurfaceMatchSeverityV2::Warning,
                    attention_level: SurfaceAttentionLevelV2::Normal,
                    can_continue: SurfaceCanContinueTrue,
                    confirmation_reasons: vec![
                        SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches,
                    ],
                    candidate: SurfaceMatchCandidateV2::Headword {
                        candidate_ref: format!("v3:surface:{}", resolved.match_id),
                        candidate_word_id: None,
                        surface: spelling.to_owned(),
                        normalized_surface: normalized.key,
                        dialect,
                        entry_kind: EntryKind::Word,
                    },
                    existing,
                })
            })
            .collect()
    }

    fn synthetic_contexts(&self) -> Vec<MatchedEntryContextV2> {
        self.contexts.iter().map(v3_context_to_v2).collect()
    }
}

fn v3_context_to_v2(context: &MatchedEntryContextV3) -> MatchedEntryContextV2 {
    MatchedEntryContextV2 {
        word_id: context.entry_id,
        pos_labels: context.pos_labels.clone(),
        gloss_previews: context.gloss_previews.clone(),
        updated_at: context.updated_at,
        inbound_relations: RelationReferenceSummaryV2 {
            total: context.inbound_relations.total,
            by_type: context.inbound_relations.by_type.clone(),
            previews: context
                .inbound_relations
                .previews
                .iter()
                .map(|preview| RelationReferencePreviewV2 {
                    source_word_id: preview.source_entry_id,
                    source_headword: preview.source_presentation.label.clone(),
                    source_status: preview.source_status,
                    relation: preview.relation,
                })
                .collect(),
            truncated: context.inbound_relations.truncated,
        },
    }
}

impl V3RelationSummaryBuilder {
    fn push(&mut self, preview: &RelationReferencePreviewV2, presentation: &EntryPresentationV3) {
        self.total = self.total.saturating_add(1);
        match preview.relation {
            RelationTypeV2::Synonym => self.synonym = self.synonym.saturating_add(1),
            RelationTypeV2::Antonym => self.antonym = self.antonym.saturating_add(1),
            RelationTypeV2::Derivative => self.derivative = self.derivative.saturating_add(1),
        }
        if self.previews.len() < 5 {
            self.previews.push(RelationReferencePreviewV3 {
                source_entry_id: preview.source_word_id,
                source_presentation: presentation.clone(),
                source_status: preview.source_status,
                relation: preview.relation,
            });
        }
    }

    fn finish(self) -> RelationReferenceSummaryV3 {
        RelationReferenceSummaryV3 {
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

fn empty_v3_relation_summary() -> RelationReferenceSummaryV3 {
    V3RelationSummaryBuilder::default().finish()
}

fn v3_sense_gloss(sense: &WordSenseV3) -> String {
    sense
        .definitions
        .iter()
        .find_map(|definition| match definition {
            WordDefinitionV3::ZhDefinition { content, .. }
            | WordDefinitionV3::ZhSentence { content, .. } => Some(content.text().to_owned()),
            WordDefinitionV3::EnDefinition { .. } | WordDefinitionV3::EnSentence { .. } => None,
        })
        .unwrap_or_default()
}

#[derive(Debug)]
pub(super) struct V3FormsSurfaceConfirmation {
    pub(super) verified_surface: Option<VerifiedSurfaceConfirmation>,
    pub(super) verified_impact: Option<VerifiedSurfaceConfirmation>,
    pub(super) evidence: Option<FormsSurfaceAcknowledgementRecord>,
}

impl LexiconService {
    /// Build the V3 half of one restore command without issuing a second
    /// snapshot. The lifecycle service merges this contribution with its V2
    /// visibility items, then signs exactly one batch-level token.
    pub(super) async fn v3_restore_surface_contribution(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        pending: &[(EntryLifecycleTarget, AdminWordAny, bool)],
    ) -> Result<V3RestoreSurfaceContribution, LexiconServiceError> {
        let mut contribution = V3RestoreSurfaceContribution::default();
        for (target, word, already_active) in pending {
            let AdminWordAny::V3(word) = word else {
                continue;
            };
            if *already_active {
                continue;
            }
            let mut keys = forms_surface_keys(word.id, &word.forms)?;
            let current_publication_keys = current_publication_surface_keys_v3(tx, word.id).await?;
            keys.extend(
                current_publication_keys
                    .iter()
                    .map(|key| V3SurfaceQueryKey {
                        dialect_scope: key.dialect_scope.clone(),
                        normalized_surface: key.normalized_surface.clone(),
                    }),
            );
            keys.sort();
            keys.dedup();
            let material = self
                .v3_surface_material_in(tx, &keys, Some(word.id), true)
                .await?;
            let synthetic_items = material.synthetic_items()?;
            if synthetic_items.len() != material.matches.len() {
                return Err(invariant_record());
            }
            for (resolved, mut synthetic) in material.matches.iter().zip(synthetic_items) {
                let match_id = format!("restore:{}:{}", word.id, resolved.match_id);
                synthetic.match_id.clone_from(&match_id);
                if let SurfaceMatchCandidateV2::Headword {
                    candidate_ref,
                    candidate_word_id,
                    ..
                } = &mut synthetic.candidate
                {
                    *candidate_ref = format!("v3:restore:{}:{candidate_ref}", word.id);
                    *candidate_word_id = Some(word.id);
                }
                contribution.items.push(synthetic);
                contribution.page_items.push(V3SurfaceSnapshotItem {
                    match_id,
                    item: resolved.item.clone(),
                });
            }
            contribution.candidate_evidence.push(serde_json::json!({
                "schema_version": 3,
                "entry_id": word.id,
                "base_revision": target.base_revision,
                "base_lifecycle_revision": target.base_lifecycle_revision,
                "current_revision": word.revision,
                "current_lifecycle_revision": word.lifecycle_revision,
                "published_revision": word.published_revision,
                "forms_content_digest": canonical_v3_forms_digest(&word.forms)?,
                "current_publication_surface_keys": current_publication_keys.iter().map(|key| {
                    serde_json::json!({
                        "language": key.language,
                        "dialect_scope": key.dialect_scope,
                        "normalized_surface": key.normalized_surface,
                    })
                }).collect::<Vec<_>>(),
                "candidate_surface_keys": keys.iter().map(|key| {
                    serde_json::json!({
                        "dialect_scope": key.dialect_scope,
                        "normalized_surface": key.normalized_surface,
                    })
                }).collect::<Vec<_>>(),
            }));
        }
        contribution
            .items
            .sort_by(|left, right| left.match_id.cmp(&right.match_id));
        contribution
            .page_items
            .sort_by(|left, right| left.match_id.cmp(&right.match_id));
        contribution
            .candidate_evidence
            .sort_by(|left, right| left["entry_id"].as_str().cmp(&right["entry_id"].as_str()));
        Ok(contribution)
    }

    /// Build collision items for the immutable V2 publication surfaces that
    /// become visible again during restore. Draft content may have diverged
    /// from the active publication, so the lifecycle service must merge these
    /// items with its draft-derived matches before signing the one restore
    /// snapshot.
    pub(super) async fn v2_restore_publication_surface_contribution(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        pending: &[(EntryLifecycleTarget, AdminWordAny, bool)],
        publication_sources: &[crate::lexicon::repository::SurfaceProjectionSource],
    ) -> Result<V2RestorePublicationSurfaceContribution, LexiconServiceError> {
        let restoring_ids = pending
            .iter()
            .filter_map(|(_, word, already_active)| match (word, already_active) {
                (AdminWordAny::V2(word), false) => Some(word.id),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let candidates = v2_restore_publication_candidates(publication_sources, &restoring_ids)?;
        let keys = candidates
            .iter()
            .flat_map(|candidate| candidate.lookup_keys.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let material = self.v3_surface_material_in(tx, &keys, None, true).await?;
        let synthetic = material.synthetic_items()?;
        if synthetic.len() != material.matches.len() {
            return Err(invariant_record());
        }
        let mut items = BTreeMap::new();
        for candidate in &candidates {
            for (resolved, existing_item) in material.matches.iter().zip(&synthetic) {
                if existing_item.existing.word_id == candidate.entry_id
                    || candidate.lookup_keys.is_disjoint(&resolved.matched_keys)
                {
                    continue;
                }
                let candidate_wire = v2_restore_publication_candidate_wire(candidate)?;
                let category = v2_restore_publication_match_category(
                    candidate.source_kind,
                    candidate.entry_kind,
                    &existing_item.existing,
                )?;
                let match_id = format!(
                    "restore:{}:v2-publication:{}",
                    candidate.entry_id,
                    hash_serializable(&serde_json::json!({
                        "candidate": candidate_wire,
                        "existing": existing_item.existing,
                        "normalization_version":
                            crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
                    }))?
                );
                items
                    .entry(match_id.clone())
                    .or_insert_with(|| LexiconSurfaceMatchV2 {
                        match_id,
                        match_category: category,
                        severity: SurfaceMatchSeverityV2::Warning,
                        attention_level: if category == SurfaceMatchCategoryV2::ExactHeadword {
                            SurfaceAttentionLevelV2::High
                        } else {
                            SurfaceAttentionLevelV2::Normal
                        },
                        can_continue: SurfaceCanContinueTrue,
                        confirmation_reasons: vec![
                            SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches,
                        ],
                        candidate: candidate_wire,
                        existing: existing_item.existing.clone(),
                    });
            }
        }
        let items = items.into_values().collect::<Vec<_>>();
        let matched_entry_ids = items
            .iter()
            .map(|item| item.existing.word_id)
            .collect::<HashSet<_>>();
        let contexts = material
            .synthetic_contexts()
            .into_iter()
            .filter(|context| matched_entry_ids.contains(&context.word_id))
            .collect();
        Ok(V2RestorePublicationSurfaceContribution { items, contexts })
    }

    /// Convert the final mixed V2/V3 synthetic membership into the strict V3
    /// public union. Existing native V3 nodes are reloaded from the authoritative
    /// projection so form/group/variant UUIDs are never guessed from V2 slots.
    pub(super) async fn v3_restore_page_data(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        items: &[LexiconSurfaceMatchV2],
        contributed: &[V3SurfaceSnapshotItem],
    ) -> Result<V3SurfaceSnapshotPageData, LexiconServiceError> {
        let contributed = contributed
            .iter()
            .map(|item| (item.match_id.as_str(), &item.item))
            .collect::<HashMap<_, _>>();
        let mut page_items = Vec::with_capacity(items.len());
        for item in items {
            let public = if let Some(public) = contributed.get(item.match_id.as_str()) {
                (*public).clone()
            } else {
                self.v3_public_item_from_synthetic(tx, &item.existing)
                    .await?
            };
            page_items.push(V3SurfaceSnapshotItem {
                match_id: item.match_id.clone(),
                item: public,
            });
        }
        let entry_ids = page_items
            .iter()
            .map(|item| surface_match_item_entry_id(&item.item))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let matched_entry_contexts = self.v3_surface_contexts_in(tx, &entry_ids).await?;
        Ok(V3SurfaceSnapshotPageData {
            items: page_items,
            matched_entry_contexts,
        })
    }

    async fn v3_public_item_from_synthetic(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        existing: &ExistingSurfaceMatchV2,
    ) -> Result<SurfaceMatchItemV3, LexiconServiceError> {
        let (source_id, content_scope) = match &existing.source {
            ExistingSurfaceSourceV2::Headword {
                source_id,
                content_scope,
                ..
            }
            | ExistingSurfaceSourceV2::Form {
                source_id,
                content_scope,
                ..
            } => (Some(source_id.as_str()), *content_scope),
            ExistingSurfaceSourceV2::Relation { content_scope, .. } => (None, *content_scope),
        };
        let record = if let Some(source_id) = source_id {
            sqlx::query_as::<_, V3RestorePublicSurfaceRecord>(
                r#"
                SELECT source.content_schema_version,
                       CASE
                           WHEN entry.archived_at IS NOT NULL THEN 'archived'
                           WHEN entry.current_publication_id IS NOT NULL THEN 'published'
                           ELSE 'draft'
                       END AS lifecycle_status,
                       source.publication_id, source.pos_id, source.group_ids,
                       source.form_id, source.variant_id, source.form_type,
                       source.dialect, source.surface
                FROM lexicon.surface_sources source
                JOIN lexicon.entries entry ON entry.id = source.entry_id
                WHERE source.entry_id = $1
                  AND source.source_id = $2
                  AND source.content_scope = $3
                  AND source.is_deleted = FALSE
                  AND (
                      source.content_scope = 'draft'
                      OR source.publication_id = entry.current_publication_id
                  )
                ORDER BY source.content_schema_version DESC,
                         source.dialect_scope, source.event_offset DESC
                LIMIT 1
                "#,
            )
            .bind(existing.word_id)
            .bind(source_id)
            .bind(surface_content_scope_str(content_scope))
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?
        } else {
            None
        };
        if let Some(record) = record.as_ref()
            && record.content_schema_version == 3
        {
            return Ok(SurfaceMatchItemV3::FormVariantV3(FormSurfaceMatchV3 {
                source_schema_version: 3,
                entry_id: existing.word_id,
                status: parse_surface_status(&record.lifecycle_status)?,
                content_scope,
                publication_id: record.publication_id,
                pos_id: record.pos_id.ok_or_else(invariant_record)?,
                group_ids: record.group_ids.clone().ok_or_else(invariant_record)?,
                form_id: record.form_id.ok_or_else(invariant_record)?,
                variant_id: record.variant_id.ok_or_else(invariant_record)?,
                form_type: parse_form_type_v3(
                    record.form_type.as_deref().ok_or_else(invariant_record)?,
                )?,
                dialect: parse_v3_dialect(&record.dialect)?,
                spelling: record.surface.clone(),
            }));
        }
        if record
            .as_ref()
            .is_some_and(|record| record.content_schema_version != 2)
        {
            return Err(invariant_record());
        }
        let publication_id = match (&existing.source, content_scope) {
            (_, SurfaceContentScopeV2::Draft) => None,
            (
                ExistingSurfaceSourceV2::Relation {
                    referencing_word_id,
                    ..
                },
                SurfaceContentScopeV2::CurrentPublication,
            ) => current_publication_id(tx, *referencing_word_id).await?,
            (_, SurfaceContentScopeV2::CurrentPublication) => record
                .as_ref()
                .and_then(|record| record.publication_id)
                .or(current_publication_id(tx, existing.word_id).await?),
        };
        Ok(SurfaceMatchItemV3::LegacyV2(LegacySurfaceMatchV3 {
            source_schema_version: 2,
            existing: existing.clone(),
            publication_id,
        }))
    }

    pub(super) async fn detect_v3_surface_warning(
        &self,
        actor_id: Uuid,
        detection_id: Uuid,
        normalized_surface: &str,
    ) -> Result<(Vec<SurfaceMatchItemV3>, Option<SurfaceMatchPageV3>), LexiconServiceError> {
        let keys = detection_surface_keys(normalized_surface);
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let material = self
            .v3_surface_material_in(&mut transaction, &keys, None, false)
            .await?;
        transaction.commit().await.map_err(database_error)?;
        if material.matches.is_empty() {
            return Ok((Vec::new(), None));
        }
        let policy = self
            .surface_policies
            .policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
            .await
            .map_err(LexiconServiceError::SurfacePolicy)?;
        let (binding, owner_bundle) = detection_surface_binding(
            actor_id,
            detection_id,
            normalized_surface,
            &material,
            policy,
        )?;
        let snapshot = self
            .create_v3_surface_snapshot(binding, owner_bundle, &material, false, policy)
            .await?;
        Ok((material.public_matches(), Some(snapshot)))
    }

    pub(super) async fn verify_v3_detection_surface_for_create(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        detection_id: Uuid,
        normalized_surface: &str,
        token: Option<&str>,
    ) -> Result<Option<VerifiedSurfaceConfirmation>, LexiconServiceError> {
        let keys = detection_surface_keys(normalized_surface);
        LexiconRepository::lock_surface_policy_writer(tx)
            .await
            .map_err(repository_error)?;
        LexiconRepository::lock_surface_keys(tx, &surface_lock_keys_v3(&keys))
            .await
            .map_err(repository_error)?;
        let material = self.v3_surface_material_in(tx, &keys, None, true).await?;
        if material.matches.is_empty() {
            let Some(token) = token else {
                return Ok(None);
            };
            self.verify_v3_surface_owner(
                token,
                actor_id,
                SurfaceConsumptionCommand::CreateEntry,
                detection_id.to_string(),
            )
            .await?;
            return Err(LexiconServiceError::SurfaceMatchesChangedWithoutSnapshot);
        }
        let policy = self
            .surface_policies
            .policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
            .await
            .map_err(LexiconServiceError::SurfacePolicy)?;
        let (binding, owner_bundle) = detection_surface_binding(
            actor_id,
            detection_id,
            normalized_surface,
            &material,
            policy,
        )?;
        let Some(token) = token else {
            let page = self
                .create_v3_surface_snapshot(binding, owner_bundle, &material, false, policy)
                .await?;
            return Err(LexiconServiceError::SurfaceMatchAcknowledgementRequiredV3(
                Box::new(page),
            ));
        };
        self.verify_v3_surface_token(token, binding, owner_bundle, &material, false, policy)
            .await
            .map(Some)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn preview_v3_forms_surface_warning(
        &self,
        actor_id: Uuid,
        entry_id: Uuid,
        base_revision: i64,
        content: &DraftFormsStepContentV3,
        affected: &[FormsImpactItemV3],
    ) -> Result<Option<SurfaceMatchPageV3>, LexiconServiceError> {
        let keys = forms_surface_keys(entry_id, content)?;
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let material = self
            .v3_surface_material_in(&mut transaction, &keys, Some(entry_id), false)
            .await?;
        transaction.commit().await.map_err(database_error)?;
        if material.matches.is_empty() {
            return Ok(None);
        }
        let policy = self
            .surface_policies
            .policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
            .await
            .map_err(LexiconServiceError::SurfacePolicy)?;
        let content_digest = canonical_v3_forms_digest(content)?;
        let evidence = self
            .repository
            .forms_surface_acknowledgement_by_entry(entry_id)
            .await
            .map_err(repository_error)?;
        if v3_forms_evidence_reusable(
            evidence.as_ref(),
            entry_id,
            &content_digest,
            &material.match_ids(),
            policy,
        ) {
            return Ok(None);
        }
        let (binding, owner_bundle) = forms_surface_binding_v3(
            actor_id,
            entry_id,
            base_revision,
            &content_digest,
            affected,
            &material,
            policy,
        )?;
        self.create_v3_surface_snapshot(
            binding,
            owner_bundle,
            &material,
            !affected.is_empty(),
            policy,
        )
        .await
        .map(Some)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn verify_v3_forms_surface_for_save(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        entry_id: Uuid,
        base_revision: i64,
        next_revision: i64,
        previous: &DraftFormsStepContentV3,
        content: &DraftFormsStepContentV3,
        affected: &[FormsImpactItemV3],
        surface_token: Option<&str>,
        impact_token: Option<Uuid>,
    ) -> Result<V3FormsSurfaceConfirmation, LexiconServiceError> {
        let previous_keys = forms_surface_keys(entry_id, previous)?;
        let keys = forms_surface_keys(entry_id, content)?;
        let lock_keys = previous_keys
            .iter()
            .chain(keys.iter())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        LexiconRepository::lock_surface_policy_writer(tx)
            .await
            .map_err(repository_error)?;
        LexiconRepository::lock_surface_keys(tx, &surface_lock_keys_v3(&lock_keys))
            .await
            .map_err(repository_error)?;
        let material = self
            .v3_surface_material_in(tx, &keys, Some(entry_id), true)
            .await?;
        let content_digest = canonical_v3_forms_digest(content)?;
        let previous_evidence = LexiconRepository::forms_surface_acknowledgement(tx, entry_id)
            .await
            .map_err(repository_error)?;
        let current_policy = if material.matches.is_empty() && surface_token.is_none() {
            None
        } else {
            Some(
                self.surface_policies
                    .policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
                    .await
                    .map_err(LexiconServiceError::SurfacePolicy)?,
            )
        };
        let reusable = current_policy.is_some_and(|policy| {
            v3_forms_evidence_reusable(
                previous_evidence.as_ref(),
                entry_id,
                &content_digest,
                &material.match_ids(),
                policy,
            )
        });
        let mut verified_surface = None;
        let mut verified_impact = None;
        if !material.matches.is_empty() && !reusable {
            let policy = current_policy.expect("non-empty matches load the policy");
            let (binding, owner_bundle) = forms_surface_binding_v3(
                actor_id,
                entry_id,
                base_revision,
                &content_digest,
                affected,
                &material,
                policy,
            )?;
            let Some(token) = surface_token else {
                let page = self
                    .create_v3_surface_snapshot(
                        binding,
                        owner_bundle,
                        &material,
                        !affected.is_empty(),
                        policy,
                    )
                    .await?;
                return Err(LexiconServiceError::SurfaceMatchAcknowledgementRequiredV3(
                    Box::new(page),
                ));
            };
            let confirmation = self
                .verify_v3_surface_token(
                    token,
                    binding.clone(),
                    owner_bundle,
                    &material,
                    !affected.is_empty(),
                    policy,
                )
                .await?;
            if !affected.is_empty() {
                let impact = impact_token.ok_or_else(|| downstream_required_v3(affected))?;
                let impact_confirmation = self
                    .verify_v3_surface_impact(impact, actor_id, entry_id, affected)
                    .await?;
                if impact_confirmation.snapshot_id != confirmation.snapshot_id
                    || impact_confirmation.binding != binding
                {
                    return Err(downstream_required_v3(affected));
                }
                verified_impact = Some(impact_confirmation);
            }
            verified_surface = Some(confirmation);
        } else if material.matches.is_empty()
            && let Some(token) = surface_token
        {
            let confirmation = self
                .verify_v3_surface_owner(
                    token,
                    actor_id,
                    SurfaceConsumptionCommand::SaveForms,
                    entry_id.to_string(),
                )
                .await?;
            let _ = confirmation;
            return Err(LexiconServiceError::SurfaceMatchesChangedWithoutSnapshot);
        }
        if !affected.is_empty() && verified_impact.is_none() {
            let token = impact_token.ok_or_else(|| downstream_required_v3(affected))?;
            let expected_content_hash = sha256_json(content).map_err(serialization_error)?;
            let confirmation = self
                .impacts
                .load(actor_id, token)
                .await
                .map_err(LexiconServiceError::ImpactStore)?;
            if confirmation.as_ref().is_none_or(|confirmation| {
                confirmation.entry_id != entry_id
                    || confirmation.base_revision != base_revision
                    || confirmation.content_hash != expected_content_hash
            }) {
                return Err(downstream_required_v3(affected));
            }
        }
        let evidence = if material.matches.is_empty() {
            None
        } else {
            let policy = current_policy.expect("non-empty matches load the policy");
            let (acknowledged_by_admin_id, acknowledged_at) = if verified_surface.is_some() {
                (actor_id, Utc::now())
            } else {
                let evidence = previous_evidence.as_ref().ok_or_else(invariant_record)?;
                (evidence.acknowledged_by_admin_id, evidence.acknowledged_at)
            };
            let items = material.synthetic_items()?;
            Some(FormsSurfaceAcknowledgementRecord {
                entry_id,
                forms_revision: next_revision,
                forms_content_digest: content_digest,
                match_ids: material.match_ids(),
                match_digest: crate::lexicon::surface_snapshot::surface_match_digest(
                    &items,
                    &[SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches],
                )
                .map_err(LexiconServiceError::SurfaceSnapshot)?,
                acknowledged_by_admin_id,
                acknowledged_at,
                policy_name: "surface_warning_acknowledgement".to_owned(),
                policy_epoch: i64::try_from(policy.epoch).map_err(|_| invariant_record())?,
                normalization_version: i32::from(
                    crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
                ),
            })
        };
        Ok(V3FormsSurfaceConfirmation {
            verified_surface,
            verified_impact,
            evidence,
        })
    }

    /// Confirm the surface set that will become the current V3 publication.
    ///
    /// Call this before locking the entry row so every publication writer keeps
    /// the shared context -> policy -> surface-key -> matched-context lock order.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn confirm_v3_publish_surface(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        entry_id: Uuid,
        base_revision: i64,
        forms: &DraftFormsStepContentV3,
        token: Option<&str>,
    ) -> Result<Option<VerifiedSurfaceConfirmation>, LexiconServiceError> {
        let keys = forms_surface_keys(entry_id, forms)?;
        self.lock_v3_publication_surface_set(tx, entry_id, &keys)
            .await?;
        let material = self
            .v3_surface_material_in(tx, &keys, Some(entry_id), true)
            .await?;
        if material.matches.is_empty() {
            let Some(token) = token else {
                return Ok(None);
            };
            self.verify_v3_surface_owner(
                token,
                actor_id,
                SurfaceConsumptionCommand::PublishEntry,
                entry_id.to_string(),
            )
            .await?;
            return Err(LexiconServiceError::SurfaceMatchesChangedWithoutSnapshot);
        }
        let policy = self
            .surface_policies
            .policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
            .await
            .map_err(LexiconServiceError::SurfacePolicy)?;
        let content_digest = canonical_v3_forms_digest(forms)?;
        let evidence = LexiconRepository::forms_surface_acknowledgement(tx, entry_id)
            .await
            .map_err(repository_error)?;
        if v3_forms_evidence_reusable(
            evidence.as_ref(),
            entry_id,
            &content_digest,
            &material.match_ids(),
            policy,
        ) {
            return Ok(None);
        }
        let (binding, owner_bundle) = owner_binding(
            actor_id,
            SurfaceConsumptionCommand::PublishEntry,
            entry_id.to_string(),
            Some(base_revision),
            content_digest.clone(),
            serde_json::json!({
                "owner_kind": "v3_publish",
                "entry_id": entry_id,
                "base_revision": base_revision,
                "forms_content_digest": content_digest,
                "confirmation_reasons": [
                    SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches
                ],
                V3_SURFACE_PAGE_DATA_KEY: material.page_data(),
            }),
            policy,
        )?;
        self.require_v3_command_surface_confirmation(
            token,
            binding,
            owner_bundle,
            &material,
            policy,
        )
        .await
        .map(Some)
    }

    /// Confirm the surfaces carried by the historical V3 publication selected
    /// for activation. The immutable snapshot itself is part of the signed
    /// digest, while owner_context binds the token to the exact target
    /// publication rather than merely to the entry.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn confirm_v3_activation_surface(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        entry_id: Uuid,
        target_publication_id: Uuid,
        base_revision: i64,
        base_lifecycle_revision: i64,
        target_snapshot: &Value,
        token: Option<&str>,
    ) -> Result<Option<VerifiedSurfaceConfirmation>, LexiconServiceError> {
        let target: AdminWordV3 = match target_snapshot
            .get("schema_version")
            .and_then(Value::as_i64)
        {
            Some(3) => {
                serde_json::from_value(target_snapshot.clone()).map_err(serialization_error)?
            }
            Some(2) => {
                let target: AdminWordV2 =
                    serde_json::from_value(target_snapshot.clone()).map_err(serialization_error)?;
                return self
                    .confirm_v2_target_activation_surface(
                        tx,
                        actor_id,
                        entry_id,
                        target_publication_id,
                        base_revision,
                        base_lifecycle_revision,
                        target_snapshot,
                        &target,
                        token,
                    )
                    .await;
            }
            Some(version) => {
                return Err(LexiconServiceError::UnsupportedSchemaVersion(
                    i16::try_from(version).unwrap_or(-1),
                ));
            }
            None => return Err(invariant_record()),
        };
        if target.id != entry_id {
            return Err(invariant_record());
        }
        let keys = forms_surface_keys(entry_id, &target.forms)?;
        self.lock_v3_publication_surface_set(tx, entry_id, &keys)
            .await?;
        let material = self
            .v3_surface_material_in(tx, &keys, Some(entry_id), true)
            .await?;
        let owner_context = serde_json::to_string(&serde_json::json!({
            "entry_id": entry_id,
            "target_publication_id": target_publication_id,
        }))
        .map_err(serialization_error)?;
        if material.matches.is_empty() {
            let Some(token) = token else {
                return Ok(None);
            };
            self.verify_v3_surface_owner(
                token,
                actor_id,
                SurfaceConsumptionCommand::ActivatePublication,
                owner_context,
            )
            .await?;
            return Err(LexiconServiceError::SurfaceMatchesChangedWithoutSnapshot);
        }
        let policy = self
            .surface_policies
            .policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
            .await
            .map_err(LexiconServiceError::SurfacePolicy)?;
        let snapshot_digest = hash_serializable(target_snapshot)?;
        let (binding, owner_bundle) = owner_binding(
            actor_id,
            SurfaceConsumptionCommand::ActivatePublication,
            owner_context,
            Some(base_revision),
            snapshot_digest.clone(),
            serde_json::json!({
                "owner_kind": "v3_activate_publication",
                "entry_id": entry_id,
                "target_publication_id": target_publication_id,
                "base_revision": base_revision,
                "base_lifecycle_revision": base_lifecycle_revision,
                "target_snapshot_digest": snapshot_digest,
                "confirmation_reasons": [
                    SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches
                ],
                V3_SURFACE_PAGE_DATA_KEY: material.page_data(),
            }),
            policy,
        )?;
        self.require_v3_command_surface_confirmation(
            token,
            binding,
            owner_bundle,
            &material,
            policy,
        )
        .await
        .map(Some)
    }

    #[allow(clippy::too_many_arguments)]
    async fn confirm_v2_target_activation_surface(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        entry_id: Uuid,
        target_publication_id: Uuid,
        base_revision: i64,
        base_lifecycle_revision: i64,
        target_snapshot: &Value,
        target: &AdminWordV2,
        token: Option<&str>,
    ) -> Result<Option<VerifiedSurfaceConfirmation>, LexiconServiceError> {
        if target.id != entry_id {
            return Err(invariant_record());
        }
        let previous_sources = current_v2_publication_sources(tx, entry_id).await?;
        let proposed_sources = crate::lexicon::repository::surface_projection_sources(target)
            .map_err(surface_projection_error)?;
        LexiconRepository::lock_surface_contexts(tx, &[entry_id])
            .await
            .map_err(repository_error)?;
        LexiconRepository::lock_surface_policy_writer(tx)
            .await
            .map_err(repository_error)?;
        let mut lock_keys = current_publication_surface_keys_v3(tx, entry_id).await?;
        lock_keys.extend(crate::lexicon::repository::surface_lock_keys([
            proposed_sources.as_slice(),
        ]));
        lock_keys.sort();
        lock_keys.dedup();
        LexiconRepository::lock_surface_keys(tx, &lock_keys)
            .await
            .map_err(repository_error)?;
        let snapshot_digest = hash_serializable(target_snapshot)?;
        let command_owner = serde_json::json!({
            "entry_id": entry_id,
            "target_publication_id": target_publication_id,
            "base_revision": base_revision,
            "base_lifecycle_revision": base_lifecycle_revision,
            "target_snapshot_digest": snapshot_digest,
        });
        let confirmation = self
            .confirm_visibility_command(
                tx,
                actor_id,
                target,
                &previous_sources,
                &proposed_sources,
                token,
                SurfaceConsumptionCommand::ActivatePublication,
                "activate_publication_v3_v2_snapshot",
                command_owner.clone(),
            )
            .await?;
        if confirmation.is_none()
            && let Some(token) = token
        {
            self.verify_v3_surface_owner(
                token,
                actor_id,
                SurfaceConsumptionCommand::ActivatePublication,
                serde_json::to_string(&command_owner).map_err(serialization_error)?,
            )
            .await?;
            return Err(LexiconServiceError::SurfaceMatchesChangedWithoutSnapshot);
        }
        Ok(confirmation)
    }

    async fn lock_v3_publication_surface_set(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        proposed_keys: &[V3SurfaceQueryKey],
    ) -> Result<(), LexiconServiceError> {
        LexiconRepository::lock_surface_contexts(tx, &[entry_id])
            .await
            .map_err(repository_error)?;
        LexiconRepository::lock_surface_policy_writer(tx)
            .await
            .map_err(repository_error)?;
        let mut lock_keys = current_publication_surface_keys_v3(tx, entry_id).await?;
        lock_keys.extend(surface_lock_keys_v3(proposed_keys));
        lock_keys.sort();
        lock_keys.dedup();
        LexiconRepository::lock_surface_keys(tx, &lock_keys)
            .await
            .map_err(repository_error)
    }

    async fn require_v3_command_surface_confirmation(
        &self,
        token: Option<&str>,
        binding: SurfaceConfirmationBinding,
        owner_bundle: Value,
        material: &V3SurfaceMaterial,
        policy: SurfaceCreationPolicy,
    ) -> Result<VerifiedSurfaceConfirmation, LexiconServiceError> {
        let Some(token) = token else {
            let page = self
                .create_v3_surface_snapshot(binding, owner_bundle, material, false, policy)
                .await?;
            return Err(LexiconServiceError::SurfaceMatchAcknowledgementRequiredV3(
                Box::new(page),
            ));
        };
        self.verify_v3_surface_token(token, binding, owner_bundle, material, false, policy)
            .await
    }

    async fn create_v3_surface_snapshot(
        &self,
        binding: SurfaceConfirmationBinding,
        owner_bundle: Value,
        material: &V3SurfaceMaterial,
        issue_impact: bool,
        policy: SurfaceCreationPolicy,
    ) -> Result<SurfaceMatchPageV3, LexiconServiceError> {
        if policy.name != SurfacePolicyNameV2::SurfaceWarningAcknowledgement || !policy.enabled {
            return Err(LexiconServiceError::SurfacePolicyChanged(policy));
        }
        let input = CreateSurfaceSnapshot {
            binding,
            policy_enabled: policy.enabled,
            policy_block_code: None,
            items: material.synthetic_items()?,
            matched_entry_contexts: material.synthetic_contexts(),
            confirmation_reasons: vec![SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches],
            owner_bundle: owner_bundle.clone(),
            page_size: DEFAULT_SURFACE_PAGE_SIZE,
        };
        let snapshot = if issue_impact {
            self.surface_snapshots
                .create_with_impact_confirmation(input)
                .await
        } else {
            self.surface_snapshots.create(input).await
        }
        .map_err(LexiconServiceError::SurfaceSnapshot)?;
        surface_page_v3(snapshot.page, &owner_bundle).map_err(LexiconServiceError::SurfaceSnapshot)
    }

    #[allow(clippy::too_many_arguments)]
    async fn verify_v3_surface_token(
        &self,
        token: &str,
        binding: SurfaceConfirmationBinding,
        owner_bundle: Value,
        material: &V3SurfaceMaterial,
        issue_impact: bool,
        policy: SurfaceCreationPolicy,
    ) -> Result<VerifiedSurfaceConfirmation, LexiconServiceError> {
        match self
            .surface_snapshots
            .verify(
                token,
                &ExpectedSurfaceConfirmation {
                    binding: binding.clone(),
                    current_policy: policy,
                },
            )
            .await
        {
            Ok(confirmation) => Ok(confirmation),
            Err(SurfaceSnapshotError::Expired) => {
                Err(LexiconServiceError::SurfaceMatchSnapshotExpired)
            }
            Err(SurfaceSnapshotError::PolicyChanged(name)) => {
                let policy = self
                    .surface_policies
                    .policy(name)
                    .await
                    .map_err(LexiconServiceError::SurfacePolicy)?;
                Err(LexiconServiceError::SurfacePolicyChanged(policy))
            }
            Err(SurfaceSnapshotError::BindingMismatch) => {
                let page = self
                    .create_v3_surface_snapshot(
                        binding,
                        owner_bundle,
                        material,
                        issue_impact,
                        policy,
                    )
                    .await?;
                Err(LexiconServiceError::SurfaceMatchesChangedV3(Box::new(page)))
            }
            Err(error) => Err(LexiconServiceError::SurfaceSnapshot(error)),
        }
    }

    pub(super) async fn verify_v3_surface_owner(
        &self,
        token: &str,
        actor_id: Uuid,
        command: SurfaceConsumptionCommand,
        owner_context: String,
    ) -> Result<VerifiedSurfaceConfirmation, LexiconServiceError> {
        match self
            .surface_snapshots
            .verify_owner(
                token,
                &ExpectedSurfaceOwner {
                    actor_id,
                    command,
                    owner_context,
                },
            )
            .await
        {
            Ok(confirmation) => Ok(confirmation),
            Err(SurfaceSnapshotError::Expired | SurfaceSnapshotError::BindingMismatch) => {
                Err(LexiconServiceError::SurfaceMatchSnapshotExpired)
            }
            Err(SurfaceSnapshotError::PolicyChanged(name)) => {
                let policy = self
                    .surface_policies
                    .policy(name)
                    .await
                    .map_err(LexiconServiceError::SurfacePolicy)?;
                Err(LexiconServiceError::SurfacePolicyChanged(policy))
            }
            Err(error) => Err(LexiconServiceError::SurfaceSnapshot(error)),
        }
    }

    async fn verify_v3_surface_impact(
        &self,
        token: Uuid,
        actor_id: Uuid,
        entry_id: Uuid,
        affected: &[FormsImpactItemV3],
    ) -> Result<VerifiedSurfaceConfirmation, LexiconServiceError> {
        match self
            .surface_snapshots
            .verify_impact(
                token,
                &ExpectedSurfaceOwner {
                    actor_id,
                    command: SurfaceConsumptionCommand::SaveForms,
                    owner_context: entry_id.to_string(),
                },
            )
            .await
        {
            Ok(confirmation) => Ok(confirmation),
            Err(SurfaceSnapshotError::Expired | SurfaceSnapshotError::BindingMismatch) => {
                Err(downstream_required_v3(affected))
            }
            Err(SurfaceSnapshotError::PolicyChanged(name)) => {
                let policy = self
                    .surface_policies
                    .policy(name)
                    .await
                    .map_err(LexiconServiceError::SurfacePolicy)?;
                Err(LexiconServiceError::SurfacePolicyChanged(policy))
            }
            Err(error) => Err(LexiconServiceError::SurfaceSnapshot(error)),
        }
    }

    async fn v3_surface_material_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        keys: &[V3SurfaceQueryKey],
        excluding_entry_id: Option<Uuid>,
        lock_contexts: bool,
    ) -> Result<V3SurfaceMaterial, LexiconServiceError> {
        if keys.is_empty() {
            return Ok(V3SurfaceMaterial {
                matches: Vec::new(),
                contexts: Vec::new(),
            });
        }
        let dialect_scopes = keys
            .iter()
            .map(|key| key.dialect_scope.as_str())
            .collect::<Vec<_>>();
        let normalized_surfaces = keys
            .iter()
            .map(|key| key.normalized_surface.as_str())
            .collect::<Vec<_>>();
        let records = sqlx::query_as::<_, V3SurfaceSourceRecord>(
            r#"
            WITH requested AS (
                SELECT DISTINCT dialect_scope, normalized_surface
                FROM unnest($1::text[], $2::text[])
                     AS value(dialect_scope, normalized_surface)
            )
            SELECT DISTINCT source.content_schema_version, source.source_kind,
                   source.source_id, source.source_node_id,
                   source.dialect_scope, source.normalized_surface,
                   source.entry_id,
                   COALESCE(entry_headword.headword, presentation.label) AS entry_headword,
                   source.entry_kind,
                   CASE
                       WHEN entry.archived_at IS NOT NULL THEN 'archived'
                       WHEN entry.current_publication_id IS NOT NULL THEN 'published'
                       ELSE 'draft'
                   END AS lifecycle_status,
                   source.content_scope, source.publication_id,
                   source.pos_id, source.pos, source.group_ids,
                   source.form_id, source.variant_id, source.form_type,
                   source.dialect, source.surface
            FROM requested
            JOIN lexicon.surface_sources source
              ON source.language = 'en'
             AND source.dialect_scope = requested.dialect_scope
             AND source.normalized_surface = requested.normalized_surface
             AND source.is_deleted = FALSE
            JOIN lexicon.entries entry ON entry.id = source.entry_id
            LEFT JOIN LATERAL (
                SELECT headword.surface AS headword
                FROM lexicon.surface_sources headword
                WHERE headword.entry_id = source.entry_id
                  AND headword.source_kind = 'headword'
                  AND headword.content_scope = source.content_scope
                  AND headword.publication_id IS NOT DISTINCT FROM source.publication_id
                  AND headword.dialect_scope = source.dialect_scope
                  AND headword.is_deleted = FALSE
                ORDER BY CASE
                             WHEN headword.dialect = 'common' THEN 0
                             WHEN headword.dialect = source.dialect_scope THEN 1
                             ELSE 2
                         END,
                         headword.source_id
                LIMIT 1
            ) entry_headword ON TRUE
            LEFT JOIN lexicon.entry_presentation_projection presentation
              ON presentation.entry_id = source.entry_id
             AND presentation.content_schema_version = 3
            WHERE ($3::uuid IS NULL OR source.entry_id <> $3)
              AND COALESCE(entry_headword.headword, presentation.label) IS NOT NULL
              AND (
                  source.content_scope = 'draft'
                  OR (
                      source.content_scope = 'current_publication'
                      AND source.publication_id = entry.current_publication_id
                  )
              )
            ORDER BY source.entry_id, source.content_schema_version,
                     source.source_id, source.dialect
            "#,
        )
        .bind(dialect_scopes)
        .bind(normalized_surfaces)
        .bind(excluding_entry_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)?;
        let v3_keys = records
            .iter()
            .filter(|record| record.content_schema_version == 3)
            .map(|record| {
                (
                    record.entry_id,
                    record.dialect_scope.clone(),
                    record.normalized_surface.clone(),
                )
            })
            .collect::<HashSet<_>>();
        let mut matches_by_id = HashMap::new();
        for record in records {
            if record.content_schema_version == 2
                && v3_keys.contains(&(
                    record.entry_id,
                    record.dialect_scope.clone(),
                    record.normalized_surface.clone(),
                ))
            {
                continue;
            }
            let status = parse_surface_status(&record.lifecycle_status)?;
            let content_scope = parse_surface_content_scope(&record.content_scope)?;
            let (item, pos) = match record.content_schema_version {
                3 => {
                    if record.source_kind != "form_variant" {
                        return Err(invariant_record());
                    }
                    (
                        SurfaceMatchItemV3::FormVariantV3(FormSurfaceMatchV3 {
                            source_schema_version: 3,
                            entry_id: record.entry_id,
                            status,
                            content_scope,
                            publication_id: record.publication_id,
                            pos_id: record.pos_id.ok_or_else(invariant_record)?,
                            group_ids: record.group_ids.ok_or_else(invariant_record)?,
                            form_id: record.form_id.ok_or_else(invariant_record)?,
                            variant_id: record.variant_id.ok_or_else(invariant_record)?,
                            form_type: parse_form_type_v3(
                                record.form_type.as_deref().ok_or_else(invariant_record)?,
                            )?,
                            dialect: parse_v3_dialect(&record.dialect)?,
                            spelling: record.surface,
                        }),
                        record.pos,
                    )
                }
                2 => {
                    let dialect = parse_v3_dialect(&record.dialect)?;
                    let source = match record.source_kind.as_str() {
                        "headword" => ExistingSurfaceSourceV2::Headword {
                            source_id: record.source_id,
                            content_scope,
                            surface: record.surface,
                            dialect,
                        },
                        "form" => ExistingSurfaceSourceV2::Form {
                            source_id: record.source_id,
                            source_node_id: record.source_node_id.ok_or_else(invariant_record)?,
                            content_scope,
                            surface: record.surface,
                            dialect,
                            pos_id: record.pos_id.ok_or_else(invariant_record)?,
                            pos: record.pos.ok_or_else(invariant_record)?,
                            form_type: WordFormTypeV2::try_from(
                                record.form_type.as_deref().ok_or_else(invariant_record)?,
                            )
                            .map_err(|()| invariant_record())?,
                        },
                        _ => return Err(invariant_record()),
                    };
                    (
                        SurfaceMatchItemV3::LegacyV2(LegacySurfaceMatchV3 {
                            source_schema_version: 2,
                            existing: ExistingSurfaceMatchV2 {
                                word_id: record.entry_id,
                                headword: record.entry_headword,
                                kind: parse_kind(&record.entry_kind)
                                    .ok_or_else(invariant_record)?,
                                status,
                                source,
                            },
                            publication_id: record.publication_id,
                        }),
                        None,
                    )
                }
                version => return Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
            };
            let matched_key = V3SurfaceQueryKey {
                dialect_scope: record.dialect_scope,
                normalized_surface: record.normalized_surface,
            };
            let match_id = v3_match_id(&item)?;
            matches_by_id
                .entry(match_id.clone())
                .and_modify(|resolved: &mut ResolvedV3SurfaceMatch| {
                    resolved.matched_keys.insert(matched_key.clone());
                })
                .or_insert_with(|| ResolvedV3SurfaceMatch {
                    match_id,
                    item,
                    pos,
                    matched_keys: BTreeSet::from([matched_key]),
                });
        }
        let mut matches = matches_by_id.into_values().collect::<Vec<_>>();
        matches.sort_by(|left, right| left.match_id.cmp(&right.match_id));
        let entry_ids = matches
            .iter()
            .map(|item| surface_match_item_entry_id(&item.item))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if lock_contexts {
            LexiconRepository::lock_surface_contexts(tx, &entry_ids)
                .await
                .map_err(repository_error)?;
        }
        let contexts = self.v3_surface_contexts_in(tx, &entry_ids).await?;
        Ok(V3SurfaceMaterial { matches, contexts })
    }

    async fn v3_surface_contexts_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        entry_ids: &[Uuid],
    ) -> Result<Vec<MatchedEntryContextV3>, LexiconServiceError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        let records = sqlx::query_as::<_, V3SurfaceContextRecord>(
            r#"
            SELECT entry.id AS entry_id, entry.content_schema_version,
                   editor.forms, editor.meanings,
                   COALESCE(presentation.label, legacy.label) AS label,
                   COALESCE(presentation.matched_surfaces, legacy.matched_surfaces)
                       AS matched_surfaces,
                   COALESCE(presentation.strategy_version, 'legacy_v2_surface_adapter_v1')
                       AS strategy_version,
                   entry.updated_at
            FROM lexicon.entries entry
            JOIN lexicon.entry_editor_projection editor ON editor.entry_id = entry.id
            LEFT JOIN lexicon.entry_presentation_projection presentation
              ON presentation.entry_id = entry.id
             AND presentation.content_schema_version = 3
            LEFT JOIN LATERAL (
                SELECT string_agg(
                           headword,
                           ' / '
                           ORDER BY CASE dialect
                               WHEN 'common' THEN 0
                               WHEN 'uk' THEN 1
                               ELSE 2
                           END
                       ) AS label,
                       array_agg(
                           headword
                           ORDER BY CASE dialect
                               WHEN 'common' THEN 0
                               WHEN 'uk' THEN 1
                               ELSE 2
                           END
                       ) AS matched_surfaces
                FROM lexicon.entry_headwords
                WHERE entry_id = entry.id
            ) legacy ON TRUE
            WHERE entry.id = ANY($1)
              AND COALESCE(presentation.label, legacy.label) IS NOT NULL
            ORDER BY entry.id
            "#,
        )
        .bind(entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)?;
        if records.len() != entry_ids.len() {
            return Err(invariant_record());
        }
        let mut relation_summaries = self.v3_inbound_relation_summaries_in(tx, entry_ids).await?;
        records
            .into_iter()
            .map(|record| {
                let (mut pos_labels, mut gloss_previews) = match record.content_schema_version {
                    2 => {
                        let forms: DraftFormsStepContent =
                            serde_json::from_value(record.forms).map_err(serialization_error)?;
                        let meanings: DraftMeaningsStepContent =
                            serde_json::from_value(record.meanings).map_err(serialization_error)?;
                        (
                            forms.pos.into_iter().map(|pos| pos.pos).collect::<Vec<_>>(),
                            meanings
                                .pos
                                .iter()
                                .flat_map(|pos| &pos.senses)
                                .map(published_sense_gloss)
                                .filter(|gloss| !gloss.is_empty())
                                .take(5)
                                .collect::<Vec<_>>(),
                        )
                    }
                    3 => {
                        let forms: DraftFormsStepContentV3 =
                            serde_json::from_value(record.forms).map_err(serialization_error)?;
                        let meanings: DraftMeaningsStepContentV3 =
                            serde_json::from_value(record.meanings).map_err(serialization_error)?;
                        (
                            forms.pos.into_iter().map(|pos| pos.pos).collect::<Vec<_>>(),
                            meanings
                                .pos
                                .iter()
                                .flat_map(|pos| &pos.senses)
                                .map(v3_sense_gloss)
                                .filter(|gloss| !gloss.is_empty())
                                .take(5)
                                .collect::<Vec<_>>(),
                        )
                    }
                    version => {
                        return Err(LexiconServiceError::UnsupportedSchemaVersion(version));
                    }
                };
                pos_labels.sort();
                pos_labels.dedup();
                pos_labels.truncate(5);
                gloss_previews.dedup();
                Ok(MatchedEntryContextV3 {
                    entry_id: record.entry_id,
                    presentation: EntryPresentationV3 {
                        label: record.label,
                        matched_surfaces: record.matched_surfaces,
                        strategy_version: record.strategy_version,
                    },
                    pos_labels,
                    gloss_previews,
                    updated_at: record.updated_at,
                    inbound_relations: relation_summaries
                        .remove(&record.entry_id)
                        .unwrap_or_else(empty_v3_relation_summary),
                })
            })
            .collect()
    }

    async fn v3_inbound_relation_summaries_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        target_entry_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, RelationReferenceSummaryV3>, LexiconServiceError> {
        let records =
            LexiconRepository::surface_inbound_relations_in_transaction(tx, target_entry_ids)
                .await
                .map_err(repository_error)?;
        let references = inbound_relation_previews(&records)?;
        let source_entry_ids = references
            .iter()
            .map(|reference| reference.preview.source_word_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if source_entry_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let presentation_records = sqlx::query_as::<_, V3RelationSourcePresentationRecord>(
            r#"
                SELECT entry.id AS entry_id,
                       COALESCE(presentation.label, legacy.label) AS label,
                       COALESCE(presentation.matched_surfaces, legacy.matched_surfaces)
                           AS matched_surfaces,
                       COALESCE(
                           presentation.strategy_version,
                           'legacy_v2_surface_adapter_v1'
                       ) AS strategy_version
                FROM lexicon.entries entry
                LEFT JOIN lexicon.entry_presentation_projection presentation
                  ON presentation.entry_id = entry.id
                 AND presentation.content_schema_version = 3
                LEFT JOIN LATERAL (
                    SELECT string_agg(
                               headword,
                               ' / '
                               ORDER BY CASE dialect
                                   WHEN 'common' THEN 0
                                   WHEN 'uk' THEN 1
                                   ELSE 2
                               END
                           ) AS label,
                           array_agg(
                               headword
                               ORDER BY CASE dialect
                                   WHEN 'common' THEN 0
                                   WHEN 'uk' THEN 1
                                   ELSE 2
                               END
                           ) AS matched_surfaces
                    FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id
                ) legacy ON TRUE
                WHERE entry.id = ANY($1)
                  AND COALESCE(presentation.label, legacy.label) IS NOT NULL
                ORDER BY entry.id
                "#,
        )
        .bind(&source_entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)?;
        if presentation_records.len() != source_entry_ids.len() {
            return Err(invariant_record());
        }
        let presentations = presentation_records
            .into_iter()
            .map(|record| {
                (
                    record.entry_id,
                    EntryPresentationV3 {
                        label: record.label,
                        matched_surfaces: record.matched_surfaces,
                        strategy_version: record.strategy_version,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let mut summaries = HashMap::<Uuid, V3RelationSummaryBuilder>::new();
        for reference in references {
            let presentation = presentations
                .get(&reference.preview.source_word_id)
                .ok_or_else(invariant_record)?;
            summaries
                .entry(reference.target_entry_id)
                .or_default()
                .push(&reference.preview, presentation);
        }
        Ok(summaries
            .into_iter()
            .map(|(entry_id, builder)| (entry_id, builder.finish()))
            .collect())
    }
}

async fn current_publication_surface_keys_v3(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<Vec<SurfaceLockKey>, LexiconServiceError> {
    sqlx::query_as::<_, (String, String, String)>(
        r#"
        SELECT DISTINCT source.language, source.dialect_scope, source.normalized_surface
        FROM lexicon.surface_sources source
        JOIN lexicon.entries entry ON entry.id = source.entry_id
        WHERE source.entry_id = $1
          AND source.content_scope = 'current_publication'
          AND source.publication_id = entry.current_publication_id
          AND source.is_deleted = FALSE
        ORDER BY source.language, source.dialect_scope, source.normalized_surface
        "#,
    )
    .bind(entry_id)
    .fetch_all(&mut **tx)
    .await
    .map(|records| {
        records
            .into_iter()
            .map(
                |(language, dialect_scope, normalized_surface)| SurfaceLockKey {
                    language,
                    dialect_scope,
                    normalized_surface,
                },
            )
            .collect()
    })
    .map_err(database_error)
}

async fn current_v2_publication_sources(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<Vec<crate::lexicon::repository::SurfaceProjectionSource>, LexiconServiceError> {
    let current = sqlx::query_as::<_, (i16, Value)>(
        r#"
        SELECT publication.content_schema_version, publication.snapshot
        FROM lexicon.entries entry
        JOIN lexicon.entry_publications publication
          ON publication.id = entry.current_publication_id
         AND publication.entry_id = entry.id
        WHERE entry.id = $1
        "#,
    )
    .bind(entry_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    match current {
        None | Some((3, _)) => Ok(Vec::new()),
        Some((2, snapshot)) => {
            let word: AdminWordV2 =
                serde_json::from_value(snapshot).map_err(serialization_error)?;
            crate::lexicon::repository::surface_projection_sources(&word)
                .map_err(surface_projection_error)
        }
        Some((version, _)) => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
    }
}

fn v2_restore_publication_candidates(
    sources: &[crate::lexicon::repository::SurfaceProjectionSource],
    restoring_ids: &HashSet<Uuid>,
) -> Result<Vec<V2RestorePublicationCandidate>, LexiconServiceError> {
    let mut candidates = BTreeMap::<(Uuid, String), V2RestorePublicationCandidate>::new();
    for source in sources
        .iter()
        .filter(|source| restoring_ids.contains(&source.entry_id))
    {
        let dialect = parse_dialect(source.dialect).ok_or_else(invariant_record)?;
        let entry_kind = parse_kind(source.entry_kind).ok_or_else(invariant_record)?;
        let form_type = source
            .form_type
            .as_deref()
            .map(WordFormTypeV2::try_from)
            .transpose()
            .map_err(|()| invariant_record())?;
        let key = V3SurfaceQueryKey {
            dialect_scope: source.dialect_scope.to_owned(),
            normalized_surface: source.normalized_surface.clone(),
        };
        let candidate = candidates
            .entry((source.entry_id, source.source_id.clone()))
            .or_insert_with(|| V2RestorePublicationCandidate {
                entry_id: source.entry_id,
                source_id: source.source_id.clone(),
                source_kind: source.source_kind,
                source_node_id: source.source_node_id,
                entry_kind,
                dialect,
                surface: source.surface.clone(),
                normalized_surface: source.normalized_surface.clone(),
                pos_id: source.pos_id,
                pos: source.pos.clone(),
                form_type,
                lookup_keys: BTreeSet::new(),
            });
        if candidate.source_kind != source.source_kind
            || candidate.source_node_id != source.source_node_id
            || candidate.entry_kind != entry_kind
            || candidate.dialect != dialect
            || candidate.surface != source.surface
            || candidate.normalized_surface != source.normalized_surface
            || candidate.pos_id != source.pos_id
            || candidate.pos != source.pos
            || candidate.form_type != form_type
        {
            return Err(invariant_record());
        }
        candidate.lookup_keys.insert(key);
    }
    Ok(candidates.into_values().collect())
}

fn v2_restore_publication_candidate_wire(
    candidate: &V2RestorePublicationCandidate,
) -> Result<SurfaceMatchCandidateV2, LexiconServiceError> {
    match candidate.source_kind {
        "headword" => Ok(SurfaceMatchCandidateV2::Headword {
            candidate_ref: candidate.source_id.clone(),
            candidate_word_id: Some(candidate.entry_id),
            surface: candidate.surface.clone(),
            normalized_surface: candidate.normalized_surface.clone(),
            dialect: candidate.dialect,
            entry_kind: candidate.entry_kind,
        }),
        "form" => Ok(SurfaceMatchCandidateV2::Form {
            candidate_ref: candidate.source_id.clone(),
            candidate_word_id: candidate.entry_id,
            candidate_node_id: candidate.source_node_id.ok_or_else(invariant_record)?,
            surface: candidate.surface.clone(),
            normalized_surface: candidate.normalized_surface.clone(),
            dialect: candidate.dialect,
            pos_id: candidate.pos_id.ok_or_else(invariant_record)?,
            pos: candidate.pos.clone().ok_or_else(invariant_record)?,
            form_type: candidate.form_type.ok_or_else(invariant_record)?,
        }),
        _ => Err(invariant_record()),
    }
}

fn v2_restore_publication_match_category(
    candidate_kind: &str,
    candidate_entry_kind: EntryKind,
    existing: &ExistingSurfaceMatchV2,
) -> Result<SurfaceMatchCategoryV2, LexiconServiceError> {
    let existing_is_form = matches!(existing.source, ExistingSurfaceSourceV2::Form { .. });
    match candidate_kind {
        "headword" if existing_is_form => Ok(SurfaceMatchCategoryV2::HeadwordForm),
        "headword" if existing.kind == candidate_entry_kind => {
            Ok(SurfaceMatchCategoryV2::ExactHeadword)
        }
        "headword" => Ok(SurfaceMatchCategoryV2::CrossKindHeadword),
        "form" if existing_is_form => Ok(SurfaceMatchCategoryV2::FormForm),
        "form" => Ok(SurfaceMatchCategoryV2::FormHeadword),
        _ => Err(invariant_record()),
    }
}

fn detection_surface_keys(normalized_surface: &str) -> Vec<V3SurfaceQueryKey> {
    ["uk", "us"]
        .into_iter()
        .map(|dialect_scope| V3SurfaceQueryKey {
            dialect_scope: dialect_scope.to_owned(),
            normalized_surface: normalized_surface.to_owned(),
        })
        .collect()
}

fn forms_surface_keys(
    entry_id: Uuid,
    content: &DraftFormsStepContentV3,
) -> Result<Vec<V3SurfaceQueryKey>, LexiconServiceError> {
    let sources = crate::lexicon::v3_projection::form_variant_sources(entry_id, content)
        .map_err(|_| invariant_record())?;
    Ok(surface_query_keys(&sources))
}

fn surface_query_keys(sources: &[V3FormVariantSurfaceSource]) -> Vec<V3SurfaceQueryKey> {
    sources
        .iter()
        .map(|source| V3SurfaceQueryKey {
            dialect_scope: source.dialect_scope.as_str().to_owned(),
            normalized_surface: source.normalized_surface.clone(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn surface_lock_keys_v3(keys: &[V3SurfaceQueryKey]) -> Vec<SurfaceLockKey> {
    keys.iter()
        .map(|key| SurfaceLockKey {
            language: "en".to_owned(),
            dialect_scope: key.dialect_scope.clone(),
            normalized_surface: key.normalized_surface.clone(),
        })
        .collect()
}

fn detection_surface_binding(
    actor_id: Uuid,
    detection_id: Uuid,
    normalized_surface: &str,
    material: &V3SurfaceMaterial,
    policy: SurfaceCreationPolicy,
) -> Result<(SurfaceConfirmationBinding, Value), LexiconServiceError> {
    let canonical_content_digest =
        hash_serializable(&("v3_detection", detection_id, normalized_surface))?;
    owner_binding(
        actor_id,
        SurfaceConsumptionCommand::CreateEntry,
        detection_id.to_string(),
        None,
        canonical_content_digest,
        serde_json::json!({
            "owner_kind": "v3_detection",
            "detection_id": detection_id,
            "normalized_surface": normalized_surface,
            V3_SURFACE_PAGE_DATA_KEY: material.page_data(),
        }),
        policy,
    )
}

fn forms_surface_binding_v3(
    actor_id: Uuid,
    entry_id: Uuid,
    base_revision: i64,
    content_digest: &str,
    affected: &[FormsImpactItemV3],
    material: &V3SurfaceMaterial,
    policy: SurfaceCreationPolicy,
) -> Result<(SurfaceConfirmationBinding, Value), LexiconServiceError> {
    owner_binding(
        actor_id,
        SurfaceConsumptionCommand::SaveForms,
        entry_id.to_string(),
        Some(base_revision),
        content_digest.to_owned(),
        serde_json::json!({
            "owner_kind": "v3_forms",
            "entry_id": entry_id,
            "base_revision": base_revision,
            "forms_content_digest": content_digest,
            "affected": affected,
            V3_SURFACE_PAGE_DATA_KEY: material.page_data(),
        }),
        policy,
    )
}

#[allow(clippy::too_many_arguments)]
fn owner_binding(
    actor_id: Uuid,
    command: SurfaceConsumptionCommand,
    owner_context: String,
    base_revision: Option<i64>,
    canonical_content_digest: String,
    owner_bundle: Value,
    policy: SurfaceCreationPolicy,
) -> Result<(SurfaceConfirmationBinding, Value), LexiconServiceError> {
    let owner_evidence_digest =
        surface_owner_bundle_digest(&owner_bundle).map_err(serialization_error)?;
    Ok((
        SurfaceConfirmationBinding {
            actor_id,
            command,
            owner_context,
            base_revision,
            canonical_content_digest,
            owner_evidence_digest,
            normalization_version: crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
            policy_name: policy.name,
            policy_epoch: policy.epoch,
        },
        owner_bundle,
    ))
}

fn v3_forms_evidence_reusable(
    evidence: Option<&FormsSurfaceAcknowledgementRecord>,
    entry_id: Uuid,
    content_digest: &str,
    current_match_ids: &[String],
    policy: SurfaceCreationPolicy,
) -> bool {
    let Some(evidence) = evidence else {
        return false;
    };
    let mut evidence_ids = evidence.match_ids.clone();
    evidence_ids.sort();
    let mut current_ids = current_match_ids.to_vec();
    current_ids.sort();
    evidence.entry_id == entry_id
        && evidence.forms_content_digest == content_digest
        && evidence_ids == current_ids
        && evidence.policy_name == "surface_warning_acknowledgement"
        && evidence.policy_epoch == i64::try_from(policy.epoch).unwrap_or_default()
        && evidence.normalization_version
            == i32::from(crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION)
}

fn canonical_v3_forms_digest(
    content: &DraftFormsStepContentV3,
) -> Result<String, LexiconServiceError> {
    hash_serializable(content)
}

fn hash_serializable(value: &impl Serialize) -> Result<String, LexiconServiceError> {
    Ok(crate::platform::hash_token(
        &serde_json::to_string(value).map_err(serialization_error)?,
    ))
}

fn v3_match_id(item: &SurfaceMatchItemV3) -> Result<String, LexiconServiceError> {
    Ok(format!("v3:{}", hash_serializable(item)?))
}

const fn surface_match_item_entry_id(item: &SurfaceMatchItemV3) -> Uuid {
    match item {
        SurfaceMatchItemV3::LegacyV2(item) => item.existing.word_id,
        SurfaceMatchItemV3::FormVariantV3(item) => item.entry_id,
    }
}

const fn legacy_surface_and_dialect(source: &ExistingSurfaceSourceV2) -> (&str, Dialect) {
    match source {
        ExistingSurfaceSourceV2::Headword {
            surface, dialect, ..
        }
        | ExistingSurfaceSourceV2::Form {
            surface, dialect, ..
        }
        | ExistingSurfaceSourceV2::Relation {
            surface, dialect, ..
        } => (surface.as_str(), *dialect),
    }
}

fn parse_surface_content_scope(value: &str) -> Result<SurfaceContentScopeV2, LexiconServiceError> {
    match value {
        "draft" => Ok(SurfaceContentScopeV2::Draft),
        "current_publication" => Ok(SurfaceContentScopeV2::CurrentPublication),
        _ => Err(invariant_record()),
    }
}

const fn surface_content_scope_str(value: SurfaceContentScopeV2) -> &'static str {
    match value {
        SurfaceContentScopeV2::Draft => "draft",
        SurfaceContentScopeV2::CurrentPublication => "current_publication",
    }
}

async fn current_publication_id(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<Option<Uuid>, LexiconServiceError> {
    sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
        .bind(entry_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)
        .map(Option::flatten)
}

fn parse_form_type_v3(value: &str) -> Result<WordFormTypeV3, LexiconServiceError> {
    match value {
        "base" => Ok(WordFormTypeV3::Base),
        "third_person_singular" => Ok(WordFormTypeV3::ThirdPersonSingular),
        "present_participle" => Ok(WordFormTypeV3::PresentParticiple),
        "past_tense" => Ok(WordFormTypeV3::PastTense),
        "past_participle" => Ok(WordFormTypeV3::PastParticiple),
        "plural" => Ok(WordFormTypeV3::Plural),
        "comparative" => Ok(WordFormTypeV3::Comparative),
        "superlative" => Ok(WordFormTypeV3::Superlative),
        _ => Err(invariant_record()),
    }
}

const fn v2_form_type(value: WordFormTypeV3) -> WordFormTypeV2 {
    match value {
        WordFormTypeV3::Base => WordFormTypeV2::Base,
        WordFormTypeV3::ThirdPersonSingular => WordFormTypeV2::ThirdPersonSingular,
        WordFormTypeV3::PresentParticiple => WordFormTypeV2::PresentParticiple,
        WordFormTypeV3::PastTense => WordFormTypeV2::PastTense,
        WordFormTypeV3::PastParticiple => WordFormTypeV2::PastParticiple,
        WordFormTypeV3::Plural => WordFormTypeV2::Plural,
        WordFormTypeV3::Comparative => WordFormTypeV2::Comparative,
        WordFormTypeV3::Superlative => WordFormTypeV2::Superlative,
    }
}

fn parse_v3_dialect(value: &str) -> Result<Dialect, LexiconServiceError> {
    match value {
        "common" => Ok(Dialect::Common),
        "uk" => Ok(Dialect::Uk),
        "us" => Ok(Dialect::Us),
        _ => Err(invariant_record()),
    }
}

fn downstream_required_v3(affected: &[FormsImpactItemV3]) -> LexiconServiceError {
    LexiconServiceError::DownstreamConfirmationRequired(
        affected.iter().map(|item| item.node_id).collect(),
    )
}
