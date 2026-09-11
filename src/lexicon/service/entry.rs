use super::*;

// --- aggregate ---

// --- support ---

fn parse_relation_type(value: &str) -> Option<RelationTypeV2> {
    match value {
        "synonym" => Some(RelationTypeV2::Synonym),
        "antonym" => Some(RelationTypeV2::Antonym),
        "derivative" => Some(RelationTypeV2::Derivative),
        _ => None,
    }
}

/// 一条已解析的入站关联，附带它落在草稿还是当前发布。
pub(super) struct InboundRelationPreview {
    pub(super) target_entry_id: Uuid,
    pub(super) preview: RelationReferencePreviewV2,
}

/// 把入站关联记录一次性解析成 preview，供关联词命中行与命中词条上下文摘要共用。
///
/// current_publication 那一路的 `inbound_relation_preview` 要克隆整份发布快照再
/// 反序列化才能取到关系类型，解两遍代价明显，所以两个消费方共用同一份结果。
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
            preview,
        });
    }
    Ok(previews)
}

pub(super) fn parse_surface_status(value: &str) -> Result<AdminWordStatus, LexiconServiceError> {
    match value {
        "draft" => Ok(AdminWordStatus::Draft),
        "published" => Ok(AdminWordStatus::Published),
        "archived" => Ok(AdminWordStatus::Archived),
        _ => Err(invariant_record()),
    }
}

fn inbound_relation_preview(
    reference: &crate::lexicon::model::SurfaceInboundRelationRecord,
) -> Result<Option<RelationReferencePreviewV2>, LexiconServiceError> {
    let relation = if let Some(relation) = reference.draft_relation_type.as_deref() {
        parse_relation_type(relation)
    } else if let Some(snapshot) = reference.source_snapshot.as_ref() {
        relation_type_for_publication_snapshot(snapshot, reference.source_node_id)?
    } else {
        None
    };
    let Some(relation) = relation else {
        return Ok(None);
    };
    let source_headword = reference
        .source_presentation_label
        .clone()
        .ok_or_else(invariant_record)?;
    Ok(Some(RelationReferencePreviewV2 {
        source_word_id: reference.source_entry_id,
        source_headword,
        source_status: parse_surface_status(&reference.source_status)?,
        relation,
    }))
}

fn relation_type_for_publication_snapshot(
    snapshot: &serde_json::Value,
    node_id: Uuid,
) -> Result<Option<RelationTypeV2>, LexiconServiceError> {
    match snapshot
        .get("schema_version")
        .and_then(serde_json::Value::as_i64)
    {
        Some(3) => {
            let source: AdminWordV3 =
                serde_json::from_value(snapshot.clone()).map_err(serialization_error)?;
            Ok(source
                .meanings
                .pos
                .iter()
                .flat_map(|pos| &pos.senses)
                .flat_map(|sense| &sense.relations)
                .find(|relation| relation.id == node_id)
                .and_then(|relation| parse_relation_type(&relation.relation)))
        }
        Some(version) => Err(LexiconServiceError::UnsupportedSchemaVersion(
            i16::try_from(version).unwrap_or(-1),
        )),
        None => Err(invariant_record()),
    }
}

impl LexiconService {
    pub(super) async fn detected_headwords_v3(
        &self,
        term: &str,
        term_family: &str,
        surface: Option<RegionSurfaceRecord>,
    ) -> Result<(WordHeadwordsV2, Dialect), LexiconServiceError> {
        self.detected_headwords_with_candidates(term, term_family, surface, true)
            .await
    }

    async fn detected_headwords_with_candidates(
        &self,
        term: &str,
        term_family: &str,
        surface: Option<RegionSurfaceRecord>,
        include_region_surfaces: bool,
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
        let mut candidates = if include_region_surfaces {
            self.repository.dictionary_candidates_v3(&target_keys).await
        } else {
            self.repository.dictionary_candidates(&target_keys).await
        }
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

    pub(super) async fn catalog_context_for_reference(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        forms: &DraftFormsStepContent,
    ) -> Result<CatalogContext, LexiconServiceError> {
        let form_codes = forms
            .pos
            .iter()
            .flat_map(|p| {
                std::iter::once(p.base_form.form_type.clone()).chain(
                    p.form_groups
                        .iter()
                        .flat_map(|g| g.slots.iter().map(|s| s.form_type.clone())),
                )
            })
            .collect::<Vec<_>>();
        let configured_form_codes =
            LexiconRepository::form_types_for_reference(transaction, &form_codes)
                .await
                .map_err(repository_error)?;
        if !form_codes
            .iter()
            .all(|code| configured_form_codes.contains(code))
        {
            return Err(LexiconServiceError::InvalidField {
                field: "form_type",
                message: "form type is not in the catalog",
            });
        }
        let codes = forms
            .pos
            .iter()
            .map(|part| part.pos.clone())
            .collect::<Vec<_>>();
        // 只为拿 FOR KEY SHARE 锁：引用期间这些词性不得被删，返回行本身没有消费方。
        LexiconRepository::catalog_parts_for_reference(transaction, &codes)
            .await
            .map_err(repository_error)?;
        let sub_parts = LexiconRepository::catalog_sub_parts_for_reference(transaction)
            .await
            .map_err(repository_error)?;
        Ok(CatalogContext {
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
