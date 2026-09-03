use super::*;

use crate::lexicon::{
    dto::{
        DialectVariantRichTextSlotV3, DraftMeaningsStepContentV3, EnglishTextV3,
        PhraseComponentUsageV3, PublishedSentenceTargetCandidateV3, SentenceTargetCandidateFormV3,
        SentenceTargetMatchEvidenceV3, SentenceTargetSenseV3, WordFormTypeV3,
        WordSentenceAssociationV3,
    },
    model::{
        NewSentenceAssociation, NewSentenceAssociationSegment, PublishedFormSurfaceRecord,
        SentenceAssociationRecord, SentenceAssociationScanRecord,
    },
    node_identity::{dialect_from_name, dialect_name},
    sentence_association::{
        RESOLVER_VERSION, SentenceToken, associable_pos, codepoint_slice,
        is_contiguous_phrase_surface, is_stopword, is_storable_surface, text_hash, tokenize,
    },
    sentence_target_discovery::{
        CodepointRange as DiscoveryCodepointRange, SourceSegment as DiscoverySourceSegment,
        any_segment_intersection,
    },
};

const REPLACE_ASSOCIATIONS_SCOPE: &str = "lexicon.sentence.associations.replace";
const CLAIM_PENDING_ASSOCIATION_SCOPE: &str = "lexicon.sentence.associations.claim";

/// 例句的一侧正文。位置只在这一侧内部有意义：distinguish 例句的 uk/us 两份正文
/// 长度不同，`colour` / `color` 之后的下标就对不上了。
pub(super) struct SentenceVariant<'a> {
    pub(super) sentence_id: Uuid,
    pub(super) dialect: Dialect,
    pub(super) text: &'a str,
}

/// 把词义内容里所有例句的所有正文侧摊平。
pub(super) fn sentence_variants(meanings: &DraftMeaningsStepContent) -> Vec<SentenceVariant<'_>> {
    let mut variants = Vec::new();
    for pos in &meanings.pos {
        for sense in &pos.senses {
            for sentence in &sense.sentences {
                match &sentence.en_text {
                    EnglishTextV2::Unified { common } => variants.push(SentenceVariant {
                        sentence_id: sentence.id,
                        dialect: Dialect::Common,
                        text: common.value.text(),
                    }),
                    EnglishTextV2::Distinguish { uk, us, .. } => {
                        for (dialect, slot) in [(Dialect::Uk, uk), (Dialect::Us, us)] {
                            if let DialectVariantSlotV2::Ready { variant } = slot {
                                variants.push(SentenceVariant {
                                    sentence_id: sentence.id,
                                    dialect,
                                    text: variant.value.text(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    variants
}

/// 这一侧正文要按哪些 surface 口径去查候选。common 侧两边都查，方言侧只查自己那边。
fn lookup_scopes(dialect: Dialect) -> &'static [&'static str] {
    match dialect {
        Dialect::Common => &["uk", "us"],
        Dialect::Uk => &["uk"],
        Dialect::Us => &["us"],
    }
}

/// 把库里的关联挂回词条，并判定每条例句的 `associations_state`。
///
/// 判据是「这一侧正文的指纹是不是解析时那一份」：一致才敢把区间交给前端，
/// 否则区间指的是一份已经被改掉的正文。整条例句所有存在的侧都一致才算 `resolved`，
/// 只要有一侧对不上就整条按未解析处理——`associations` 恒为空数组，
/// 前端据此提示「正文已修改，关联将在重新发布后重新解析」。
pub(super) fn apply_sentence_associations(
    meanings: &mut DraftMeaningsStepContent,
    associations: Vec<SentenceAssociationRecord>,
    scans: Vec<SentenceAssociationScanRecord>,
) {
    let scanned = scans
        .into_iter()
        .filter(|scan| scan.resolver_version == RESOLVER_VERSION)
        .filter_map(|scan| {
            let dialect = dialect_from_name(&scan.source_dialect)?;
            Some(((scan.sentence_id, dialect), scan.text_hash))
        })
        .collect::<HashMap<_, _>>();
    let mut by_sentence: HashMap<Uuid, Vec<SentenceAssociationRecord>> = HashMap::new();
    for association in associations {
        by_sentence
            .entry(association.sentence_id)
            .or_default()
            .push(association);
    }

    for pos in &mut meanings.pos {
        for sense in &mut pos.senses {
            for sentence in &mut sense.sentences {
                let present = present_variants(&sentence.en_text);
                let resolved = !present.is_empty()
                    && present.iter().all(|(dialect, text)| {
                        scanned
                            .get(&(sentence.id, *dialect))
                            .is_some_and(|hash| hash == &text_hash(text))
                    });
                if !resolved {
                    sentence.associations = Vec::new();
                    sentence.associations_state = SentenceAssociationsStateV2::Unresolved;
                    continue;
                }
                let mut rows = by_sentence.remove(&sentence.id).unwrap_or_default();
                // 只挂这条例句现在还有的方言侧。某一侧被改成 missing 之后、下一次发布
                // prune 之前，库里仍留着那一侧的历史关联，挂上去就是在给前端一份它
                // 根本渲染不出来的正文的位置。
                rows.retain(|row| {
                    dialect_from_name(&row.source_dialect).is_some_and(|dialect| {
                        present.iter().any(|(candidate, _)| *candidate == dialect)
                    })
                });
                rows.sort_by(|left, right| {
                    left.source_dialect
                        .cmp(&right.source_dialect)
                        .then(left.range_start.cmp(&right.range_start))
                });
                sentence.associations = rows.into_iter().filter_map(association_wire).collect();
                sentence.associations_state = SentenceAssociationsStateV2::Resolved;
            }
        }
    }
}

fn present_variants(en_text: &EnglishTextV2) -> Vec<(Dialect, &str)> {
    match en_text {
        EnglishTextV2::Unified { common } => vec![(Dialect::Common, common.value.text())],
        EnglishTextV2::Distinguish { uk, us, .. } => [(Dialect::Uk, uk), (Dialect::Us, us)]
            .into_iter()
            .filter_map(|(dialect, slot)| match slot {
                DialectVariantSlotV2::Ready { variant } => Some((dialect, variant.value.text())),
                DialectVariantSlotV2::Missing => None,
            })
            .collect(),
    }
}

fn association_wire(record: SentenceAssociationRecord) -> Option<WordSentenceAssociationV2> {
    if record.segment_count != 1 {
        tracing::warn!(
            association_id = %record.id,
            sentence_id = %record.sentence_id,
            segment_count = record.segment_count,
            "refused to project a segmented association into the V2 source_range contract"
        );
        return None;
    }
    // 这三项都由库层 CHECK 保证，走到 None 说明数据坏了或迁移出了问题。
    // 静默少一条关联和「这个词本来就没关联」在界面上一模一样，排查时没有任何线索，
    // 所以至少留一条日志。
    let (Some(source_dialect), Ok(start), Ok(end)) = (
        dialect_from_name(&record.source_dialect),
        usize::try_from(record.range_start),
        usize::try_from(record.range_end),
    ) else {
        tracing::warn!(
            association_id = %record.id,
            sentence_id = %record.sentence_id,
            source_dialect = %record.source_dialect,
            "dropped a sentence association row that violates its column constraints"
        );
        return None;
    };
    let source_range = SentenceSourceRangeV1 {
        start,
        end,
        surface: record.surface,
    };
    let origin = match record.origin.as_str() {
        "manual" => SentenceAssociationOriginV2::Manual,
        _ => SentenceAssociationOriginV2::Auto,
    };
    match record.state.as_str() {
        "linked" => Some(WordSentenceAssociationV2::Linked {
            id: record.id,
            source_dialect,
            source_range,
            target_word_id: record.target_entry_id?,
            target_sense_id: record.target_sense_id?,
            target_form_slot_id: record.target_form_slot_id,
            origin,
            target_headword: record.target_headword_snapshot?,
            target_gloss: record.target_gloss_snapshot?,
            resolved_pos: record.resolved_pos?,
            resolved_form_type: record.resolved_form_type,
        }),
        "pending" => {
            let pending_target_kind = match record.pending_target_kind.as_deref() {
                Some("word") => EntryKind::Word,
                Some("phrase") => EntryKind::Phrase,
                Some(kind) => {
                    tracing::warn!(
                        association_id = %record.id,
                        sentence_id = %record.sentence_id,
                        pending_target_kind = %kind,
                        "dropped a sentence association row with an unknown pending target kind"
                    );
                    return None;
                }
                None => return None,
            };
            Some(WordSentenceAssociationV2::Pending {
                id: record.id,
                source_dialect,
                source_range,
                origin,
                pending_target_kind,
                pending_target_headword: record.pending_target_headword?,
                normalized_pending_target_headword: record.normalized_pending_target_headword?,
                pending_target_gloss: record.pending_target_gloss,
            })
        }
        _ => {
            tracing::warn!(
                association_id = %record.id,
                sentence_id = %record.sentence_id,
                state = %record.state,
                "dropped a sentence association row with an unknown state"
            );
            None
        }
    }
}

fn association_wire_v3(record: SentenceAssociationRecord) -> Option<WordSentenceAssociationV3> {
    let source_dialect = dialect_from_name(&record.source_dialect)?;
    let source_segments =
        serde_json::from_value::<Vec<SentenceSourceRangeV1>>(record.source_segments.clone())
            .ok()?;
    if source_segments.is_empty()
        || source_segments.len() != usize::try_from(record.segment_count).ok()?
    {
        tracing::warn!(
            association_id = %record.id,
            sentence_id = %record.sentence_id,
            segment_count = record.segment_count,
            "dropped a V3 sentence association with inconsistent segments"
        );
        return None;
    }
    let state = match record.state.as_str() {
        "linked" => SentenceAssociationStateV1::Linked,
        "pending" => SentenceAssociationStateV1::Pending,
        _ => return None,
    };
    let pending_target_kind = match record.pending_target_kind.as_deref() {
        Some("word") => Some(EntryKind::Word),
        Some("phrase") => Some(EntryKind::Phrase),
        None => None,
        Some(_) => return None,
    };
    let target_component_usages = record
        .target_component_usages_snapshot
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let origin = match record.origin.as_str() {
        "manual" => SentenceAssociationOriginV2::Manual,
        _ => SentenceAssociationOriginV2::Auto,
    };
    match state {
        SentenceAssociationStateV1::Linked => Some(WordSentenceAssociationV3::Linked {
            id: record.id,
            association_schema_version: 3,
            source_dialect,
            source_segments,
            target_word_id: record.target_entry_id?,
            target_sense_id: record.target_sense_id?,
            target_form_slot_id: record.target_form_slot_id,
            target_publication_id: record.target_publication_id,
            target_form_variant_id: record.target_form_variant_id,
            target_component_usages,
            origin,
            target_headword: record.target_headword_snapshot?,
            target_gloss: record.target_gloss_snapshot?,
            resolved_pos: record.resolved_pos?,
            resolved_form_type: record.resolved_form_type,
        }),
        SentenceAssociationStateV1::Pending => Some(WordSentenceAssociationV3::Pending {
            id: record.id,
            association_schema_version: 3,
            source_dialect,
            source_segments,
            origin,
            pending_target_kind: pending_target_kind?,
            pending_target_headword: record.pending_target_headword?,
            normalized_pending_target_headword: record.normalized_pending_target_headword,
            pending_target_gloss: record.pending_target_gloss,
        }),
    }
}

fn present_variants_v3(en_text: &EnglishTextV3) -> Vec<(Dialect, &str)> {
    match en_text {
        EnglishTextV3::Unified { common } => vec![(Dialect::Common, common.value.text())],
        EnglishTextV3::Distinguish { uk, us, .. } => [(Dialect::Uk, uk), (Dialect::Us, us)]
            .into_iter()
            .filter_map(|(dialect, slot)| match slot {
                DialectVariantRichTextSlotV3::Ready { variant } => {
                    Some((dialect, variant.value.text()))
                }
                DialectVariantRichTextSlotV3::Missing => None,
            })
            .collect(),
    }
}

fn apply_sentence_associations_v3(
    meanings: &mut DraftMeaningsStepContentV3,
    associations: Vec<SentenceAssociationRecord>,
    scans: Vec<SentenceAssociationScanRecord>,
) {
    let scanned = scans
        .into_iter()
        .filter(|scan| scan.resolver_version == RESOLVER_VERSION)
        .filter_map(|scan| {
            let dialect = dialect_from_name(&scan.source_dialect)?;
            Some(((scan.sentence_id, dialect), scan.text_hash))
        })
        .collect::<HashMap<_, _>>();
    let mut by_sentence: HashMap<Uuid, Vec<SentenceAssociationRecord>> = HashMap::new();
    for association in associations {
        by_sentence
            .entry(association.sentence_id)
            .or_default()
            .push(association);
    }

    for sentence in meanings
        .pos
        .iter_mut()
        .flat_map(|pos| &mut pos.senses)
        .flat_map(|sense| &mut sense.sentences)
    {
        let present = present_variants_v3(&sentence.en_text);
        let resolved = !present.is_empty()
            && present.iter().all(|(dialect, text)| {
                scanned
                    .get(&(sentence.id, *dialect))
                    .is_some_and(|hash| hash == &text_hash(text))
            });
        if !resolved {
            sentence.associations.clear();
            sentence.associations_state = SentenceAssociationsStateV2::Unresolved;
            continue;
        }
        let mut rows = by_sentence.remove(&sentence.id).unwrap_or_default();
        rows.retain(|row| {
            dialect_from_name(&row.source_dialect)
                .is_some_and(|dialect| present.iter().any(|(candidate, _)| *candidate == dialect))
        });
        rows.sort_by(|left, right| {
            left.source_dialect
                .cmp(&right.source_dialect)
                .then(left.range_start.cmp(&right.range_start))
        });
        sentence.associations = rows.into_iter().filter_map(association_wire_v3).collect();
        sentence.associations_state = SentenceAssociationsStateV2::Resolved;
    }
}

fn new_association_from_record(record: SentenceAssociationRecord) -> NewSentenceAssociation {
    let source_segments =
        serde_json::from_value::<Vec<SentenceSourceRangeV1>>(record.source_segments.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|segment| {
                Some(NewSentenceAssociationSegment {
                    range_start: i32::try_from(segment.start).ok()?,
                    range_end: i32::try_from(segment.end).ok()?,
                    surface: segment.surface,
                })
            })
            .collect::<Vec<_>>();
    NewSentenceAssociation {
        id: record.id,
        sentence_id: record.sentence_id,
        source_dialect: record.source_dialect,
        association_schema_version: record.association_schema_version,
        source_segments,
        segments_fingerprint: record.segments_fingerprint,
        range_start: record.range_start,
        range_end: record.range_end,
        surface: record.surface,
        state: record.state,
        target_entry_id: record.target_entry_id,
        target_sense_id: record.target_sense_id,
        target_form_slot_id: record.target_form_slot_id,
        target_publication_id: record.target_publication_id,
        target_form_variant_id: record.target_form_variant_id,
        target_component_usages_snapshot: record.target_component_usages_snapshot,
        origin: record.origin,
        target_headword_snapshot: record.target_headword_snapshot,
        target_gloss_snapshot: record.target_gloss_snapshot,
        resolved_pos: record.resolved_pos,
        resolved_form_type: record.resolved_form_type,
        pending_target_kind: record.pending_target_kind,
        pending_target_headword: record.pending_target_headword,
        normalized_pending_target_headword: record.normalized_pending_target_headword,
        pending_target_gloss: record.pending_target_gloss,
    }
}

/// 目标词条当前发布快照里能拿到的、一条关联需要固化的全部信息。
pub(super) struct ResolvedTarget {
    pub(super) target_entry_id: Uuid,
    pub(super) target_sense_id: Uuid,
    pub(super) target_form_slot_id: Option<Uuid>,
    pub(super) target_publication_id: Option<Uuid>,
    pub(super) target_form_variant_id: Option<Uuid>,
    pub(super) target_component_usages: Vec<PhraseComponentUsageV3>,
    pub(super) target_headword: String,
    pub(super) target_gloss: String,
    pub(super) resolved_pos: String,
    pub(super) resolved_form_type: Option<String>,
}

#[derive(Debug)]
struct PublishedAssociationForm {
    id: Uuid,
    form_type: String,
    base_form_ids: Vec<Uuid>,
    variants: Vec<PublishedAssociationVariant>,
    normalized_surfaces: Vec<String>,
}

#[derive(Debug)]
struct PublishedAssociationVariant {
    id: Uuid,
    dialect: Dialect,
    spelling: String,
    normalized_surface: Option<String>,
    component_usages: Vec<PhraseComponentUsageV3>,
}

#[derive(Debug)]
struct PublishedAssociationSense {
    id: Uuid,
    level: String,
    gloss: String,
    /// 释义级短语成分用词。V2 目标恒为空。
    component_usages: Vec<PhraseComponentUsageV3>,
}

#[derive(Debug)]
struct PublishedAssociationPos {
    id: Uuid,
    pos: String,
    forms: Vec<PublishedAssociationForm>,
    senses: Vec<PublishedAssociationSense>,
}

/// 例句关联只消费已发布词条的一小块稳定视图。先把 V2/V3 快照收敛到同一形状，
/// resolver 与人工编辑器就不会把某个 schema 的聚合 DTO 当成唯一事实来源。
#[derive(Debug)]
pub(super) struct PublishedAssociationTarget {
    schema_version: i16,
    id: Uuid,
    publication_id: Uuid,
    kind: EntryKind,
    headword: String,
    pos: Vec<PublishedAssociationPos>,
}

impl PublishedAssociationTarget {
    fn from_v2(word: AdminWordV2, publication_id: Uuid) -> Self {
        let mut pos: Vec<PublishedAssociationPos> =
            word.forms
                .pos
                .iter()
                .map(|forms| {
                    let mut slots = Vec::new();
                    slots.push(PublishedAssociationForm {
                        id: forms.base_form.id,
                        form_type: forms.base_form.form_type.clone(),
                        base_form_ids: vec![forms.base_form.id],
                        variants: forms
                            .base_form
                            .variants
                            .iter()
                            .map(|variant| PublishedAssociationVariant {
                                id: variant.id,
                                dialect: variant.dialect,
                                spelling: variant.spelling.clone(),
                                normalized_surface: normalized_surface(&variant.spelling),
                                component_usages: Vec::new(),
                            })
                            .collect(),
                        normalized_surfaces: normalized_v2_surfaces(&forms.base_form.variants),
                    });
                    slots.extend(forms.form_groups.iter().flat_map(|group| &group.slots).map(
                        |slot| {
                            PublishedAssociationForm {
                                id: slot.id,
                                form_type: slot.form_type.clone(),
                                base_form_ids: vec![forms.base_form.id],
                                variants: slot
                                    .variants
                                    .iter()
                                    .map(|variant| PublishedAssociationVariant {
                                        id: variant.id,
                                        dialect: variant.dialect,
                                        spelling: variant.spelling.clone(),
                                        normalized_surface: normalized_surface(&variant.spelling),
                                        component_usages: Vec::new(),
                                    })
                                    .collect(),
                                normalized_surfaces: normalized_v2_surfaces(&slot.variants),
                            }
                        },
                    ));
                    PublishedAssociationPos {
                        id: forms.pos_id,
                        pos: forms.pos.clone(),
                        forms: slots,
                        senses: association_senses(&word.meanings, forms.pos_id),
                    }
                })
                .collect();
        if pos.is_empty() && word.kind == EntryKind::Phrase {
            pos = word
                .meanings
                .pos
                .iter()
                .map(|meanings| PublishedAssociationPos {
                    id: meanings.pos_id,
                    pos: "phrase".to_owned(),
                    forms: Vec::new(),
                    senses: meanings
                        .senses
                        .iter()
                        .map(|sense| PublishedAssociationSense {
                            id: sense.id,
                            level: sense.level.clone(),
                            gloss: published_sense_gloss(sense),
                            component_usages: Vec::new(),
                        })
                        .collect(),
                })
                .collect();
        }
        Self {
            schema_version: 2,
            id: word.id,
            publication_id,
            kind: word.kind,
            headword: published_word_headword(&word),
            pos,
        }
    }

    fn from_v3(word: AdminWordV3, publication_id: Uuid) -> Result<Self, LexiconServiceError> {
        let meanings: DraftMeaningsStepContent = serde_json::from_value(
            serde_json::to_value(&word.meanings).map_err(serialization_error)?,
        )
        .map_err(serialization_error)?;
        let pos = word
            .forms
            .pos
            .iter()
            .map(|forms| PublishedAssociationPos {
                id: forms.pos_id,
                pos: forms.pos.clone(),
                forms: forms
                    .forms
                    .iter()
                    .map(|form| {
                        let variants = v3_form_variants(&form.regional_variants);
                        let mut base_form_ids = forms
                            .form_groups
                            .iter()
                            .filter(|group| {
                                group.members.iter().any(|member| member.form_id == form.id)
                            })
                            .flat_map(|group| &group.members)
                            .filter_map(|member| {
                                forms.forms.iter().find(|candidate| {
                                    candidate.id == member.form_id
                                        && candidate.form_type == WordFormTypeV3::Base
                                })
                            })
                            .map(|base| base.id)
                            .collect::<Vec<_>>();
                        if base_form_ids.is_empty() && form.form_type == WordFormTypeV3::Base {
                            base_form_ids.push(form.id);
                        }
                        base_form_ids.sort_unstable();
                        base_form_ids.dedup();
                        PublishedAssociationForm {
                            id: form.id,
                            form_type: v3_form_type_name(form.form_type).to_owned(),
                            base_form_ids,
                            variants: variants
                                .iter()
                                .map(|(id, dialect, spelling, component_usages)| {
                                    PublishedAssociationVariant {
                                        id: *id,
                                        dialect: *dialect,
                                        spelling: (*spelling).to_owned(),
                                        normalized_surface: normalized_surface(spelling),
                                        component_usages: component_usages.to_vec(),
                                    }
                                })
                                .collect(),
                            normalized_surfaces: variants
                                .iter()
                                .filter_map(|(_, _, spelling, _)| normalized_surface(spelling))
                                .collect(),
                        }
                    })
                    .collect(),
                senses: association_senses_v3(&word.meanings, &meanings, forms.pos_id),
            })
            .collect();
        Ok(Self {
            schema_version: 3,
            id: word.id,
            publication_id,
            kind: match word.kind {
                WordEntryKindV3::Word => EntryKind::Word,
                WordEntryKindV3::Phrase => EntryKind::Phrase,
            },
            headword: word.presentation.label,
            pos,
        })
    }

    pub(super) fn from_snapshot(
        snapshot: serde_json::Value,
        allow_v3: bool,
        publication_id: Uuid,
    ) -> Result<Self, LexiconServiceError> {
        let version = snapshot
            .get("schema_version")
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i16::try_from(value).ok())
            .unwrap_or(-1);
        match version {
            2 => Ok(Self::from_v2(
                v2_publication_snapshot(snapshot)?,
                publication_id,
            )),
            3 if allow_v3 => Self::from_v3(
                serde_json::from_value(snapshot).map_err(serialization_error)?,
                publication_id,
            ),
            3 => Err(LexiconServiceError::V3StorageUnavailable),
            version => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
        }
    }

    pub(super) fn sentence_discovery_candidates(
        &self,
        publication_id: Uuid,
        pos_id: Uuid,
        matched_form_id: Uuid,
        matched_variant_id: Uuid,
        // 关键字检索没有句子区间，传 None 即候选不带命中证据；后端不为此构造假证据。
        evidence: Option<SentenceTargetMatchEvidenceV3>,
    ) -> Vec<PublishedSentenceTargetCandidateV3> {
        let Some(pos) = self.pos.iter().find(|pos| pos.id == pos_id) else {
            return Vec::new();
        };
        let Some(form) = pos.forms.iter().find(|form| {
            form.id == matched_form_id
                || form.variants.iter().any(|variant| {
                    variant.id == matched_form_id || variant.id == matched_variant_id
                })
        }) else {
            return Vec::new();
        };
        let Some(variant) = form
            .variants
            .iter()
            .find(|variant| variant.id == matched_variant_id || variant.id == matched_form_id)
        else {
            return Vec::new();
        };
        let Some(form_type) = parse_v3_form_type_name(&form.form_type) else {
            return Vec::new();
        };
        // 短语成分只接受 V3 发布的目标（validate_phrase_components 只查 content_schema_version = 3），
        // V2 目标的词形一律给空列表：调用方按「为空不可选」处理即可，不必另辨版本。
        let component_targetable = self.schema_version == 3;
        let mut candidate_forms =
            pos.forms
                .iter()
                .filter_map(|candidate_form| {
                    let form_type = parse_v3_form_type_name(&candidate_form.form_type)?;
                    Some((candidate_form, form_type))
                })
                .flat_map(|(candidate_form, form_type)| {
                    candidate_form.variants.iter().map(move |variant| {
                        SentenceTargetCandidateFormV3 {
                            form_id: candidate_form.id,
                            variant_id: variant.id,
                            form_type,
                            spelling: variant.spelling.clone(),
                            dialect: variant.dialect,
                            base_form_ids: if component_targetable {
                                candidate_form.base_form_ids.clone()
                            } else {
                                Vec::new()
                            },
                        }
                    })
                })
                .collect::<Vec<_>>();
        let mut candidates = Vec::with_capacity(form.base_form_ids.len());
        let mut base_form_ids = form.base_form_ids.iter().peekable();
        while let Some(base_form_id) = base_form_ids.next() {
            // 词形清单在同一词性下对每个 base form 都一样，最后一个候选直接接手，不再复制。
            let forms = if base_form_ids.peek().is_some() {
                candidate_forms.clone()
            } else {
                std::mem::take(&mut candidate_forms)
            };
            candidates.push(PublishedSentenceTargetCandidateV3 {
                entry_id: self.id,
                publication_id,
                pos_id,
                base_form_id: *base_form_id,
                kind: self.kind,
                headword: self.headword.clone(),
                pos: pos.pos.clone(),
                matched_form_id: form.id,
                matched_variant_id: variant.id,
                matched_dialect: variant.dialect,
                matched_form_type: form_type,
                forms,
                component_usages: variant.component_usages.clone(),
                matches: evidence.iter().cloned().collect(),
                senses: pos
                    .senses
                    .iter()
                    .map(|sense| SentenceTargetSenseV3 {
                        sense_id: sense.id,
                        publication_id,
                        pos_id,
                        base_form_id: *base_form_id,
                        level: sense.level.clone(),
                        gloss: sense.gloss.clone(),
                        component_usages: sense.component_usages.clone(),
                    })
                    .collect(),
            });
        }
        candidates
    }

    fn automatic_target(&self, pos_id: Uuid, variant_ids: &[Uuid]) -> Option<ResolvedTarget> {
        let pos = self.pos.iter().find(|pos| pos.id == pos_id)?;
        let [sense] = pos.senses.as_slice() else {
            return None;
        };
        let slots = pos
            .forms
            .iter()
            .filter(|form| {
                form.variants
                    .iter()
                    .any(|variant| variant_ids.contains(&variant.id))
            })
            .map(|form| (form.id, form.form_type.clone()))
            .collect::<BTreeMap<_, _>>();
        let slot = (slots.len() == 1)
            .then(|| slots.into_iter().next())
            .flatten();
        Some(ResolvedTarget {
            target_entry_id: self.id,
            target_sense_id: sense.id,
            target_form_slot_id: slot.as_ref().map(|(id, _)| *id),
            target_publication_id: None,
            target_form_variant_id: None,
            target_component_usages: Vec::new(),
            target_headword: self.headword.clone(),
            target_gloss: sense.gloss.clone(),
            resolved_pos: pos.pos.clone(),
            resolved_form_type: slot.map(|(_, form_type)| form_type),
        })
    }

    fn manual_target(
        &self,
        sense_id: Uuid,
        normalized_surface: &str,
        requested_publication_id: Option<Uuid>,
        requested_variant_id: Option<Uuid>,
    ) -> Option<ResolvedTarget> {
        if requested_publication_id.is_some_and(|id| id != self.publication_id) {
            return None;
        }
        let pos = self
            .pos
            .iter()
            .find(|pos| pos.senses.iter().any(|sense| sense.id == sense_id))?;
        let sense = pos.senses.iter().find(|sense| sense.id == sense_id)?;
        let matching_variants = pos
            .forms
            .iter()
            .flat_map(|form| {
                form.variants
                    .iter()
                    .filter(move |variant| {
                        variant.normalized_surface.as_deref() == Some(normalized_surface)
                    })
                    .map(move |variant| {
                        (
                            form.id,
                            form.form_type.clone(),
                            variant.id,
                            variant.dialect,
                            variant.component_usages.clone(),
                        )
                    })
            })
            .collect::<Vec<_>>();
        let selected_variant = match requested_variant_id {
            Some(id) => matching_variants
                .iter()
                .find(|(_, _, variant_id, _, _)| *variant_id == id)
                .cloned(),
            None if matching_variants.len() == 1 => matching_variants.first().cloned(),
            None => None,
        };
        // V2 沿用历史的「按目录顺序取首个槽位」行为；V3 允许同类 concrete form
        // 重复，不能把第一个误当成管理员选择，只有唯一命中时才固化 form_id。
        let slot = if self.schema_version == 2 {
            matching_variants
                .into_iter()
                .next()
                .map(|(form_id, form_type, _, _, _)| (form_id, form_type))
        } else {
            selected_variant
                .as_ref()
                .map(|(form_id, form_type, _, _, _)| (*form_id, form_type.clone()))
        };
        if self.schema_version == 3
            && self.kind == EntryKind::Phrase
            && (requested_variant_id.is_none() || selected_variant.is_none())
        {
            return None;
        }
        let persist_exact_identity = self.schema_version == 3
            && (self.kind == EntryKind::Phrase
                || requested_publication_id.is_some()
                || requested_variant_id.is_some());
        Some(ResolvedTarget {
            target_entry_id: self.id,
            target_sense_id: sense_id,
            target_form_slot_id: slot.as_ref().map(|(id, _)| *id),
            target_publication_id: persist_exact_identity.then_some(self.publication_id),
            target_form_variant_id: if persist_exact_identity {
                selected_variant
                    .as_ref()
                    .map(|(_, _, variant_id, _, _)| *variant_id)
            } else {
                None
            },
            // 成分用词已改为释义级绑定：优先固化被选中 sense 的那一份。但 B1 期间存量短语的
            // 成分还挂在词形上，sense 侧为空时必须回退到命中词形——否则这段窗口里新建的关联
            // 会丢掉候选行上明明显示着的成分（前端也是同一套「sense 优先、缺失回退候选级」口径）。
            // B2 停掉变体侧时这条回退一并删除。
            target_component_usages: if persist_exact_identity {
                let sense_usages = sense.component_usages.clone();
                if sense_usages.is_empty() {
                    selected_variant
                        .map(|(_, _, _, _, component_usages)| component_usages)
                        .unwrap_or_default()
                } else {
                    sense_usages
                }
            } else {
                Vec::new()
            },
            target_headword: self.headword.clone(),
            target_gloss: sense.gloss.clone(),
            resolved_pos: pos.pos.clone(),
            resolved_form_type: slot.map(|(_, form_type)| form_type),
        })
    }
}

fn association_senses(
    meanings: &DraftMeaningsStepContent,
    pos_id: Uuid,
) -> Vec<PublishedAssociationSense> {
    meanings
        .pos
        .iter()
        .find(|pos| pos.pos_id == pos_id)
        .map(|pos| {
            pos.senses
                .iter()
                .map(|sense| PublishedAssociationSense {
                    id: sense.id,
                    level: sense.level.clone(),
                    gloss: published_sense_gloss(sense),
                    component_usages: Vec::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// V3 目标的词义视图。gloss 沿用 V2 形状（`published_sense_gloss` 只认 V2 定义），
/// 成分用词只有 V3 结构里才有，所以两份 meanings 都要。
fn association_senses_v3(
    meanings: &crate::lexicon::dto::DraftMeaningsStepContentV3,
    relational: &DraftMeaningsStepContent,
    pos_id: Uuid,
) -> Vec<PublishedAssociationSense> {
    let component_usages_by_sense = meanings
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .map(|sense| (sense.id, sense.component_usages.to_vec()))
        .collect::<HashMap<_, _>>();
    association_senses(relational, pos_id)
        .into_iter()
        .map(|sense| PublishedAssociationSense {
            component_usages: component_usages_by_sense
                .get(&sense.id)
                .cloned()
                .unwrap_or_default(),
            ..sense
        })
        .collect()
}

fn normalized_surface(spelling: &str) -> Option<String> {
    crate::lexicon::normalization::normalize_headword(spelling)
        .ok()
        .map(|normalized| normalized.key)
}

fn normalized_v2_surfaces(variants: &[WordFormVariantV2]) -> Vec<String> {
    variants
        .iter()
        .filter_map(|variant| normalized_surface(&variant.spelling))
        .collect()
}

fn v3_form_variants(
    variants: &crate::lexicon::dto::WordRegionalVariantsV3,
) -> Vec<(Uuid, Dialect, &str, &[PhraseComponentUsageV3])> {
    use crate::lexicon::dto::WordRegionalVariantsV3;

    match variants {
        WordRegionalVariantsV3::Common { common } => vec![(
            common.id,
            Dialect::Common,
            common.spelling.as_str(),
            &common.component_usages,
        )],
        WordRegionalVariantsV3::UkUs { uk, us } => vec![
            (
                uk.id,
                Dialect::Uk,
                uk.spelling.as_str(),
                &uk.component_usages,
            ),
            (
                us.id,
                Dialect::Us,
                us.spelling.as_str(),
                &us.component_usages,
            ),
        ],
    }
}

fn parse_v3_form_type_name(value: &str) -> Option<WordFormTypeV3> {
    Some(match value {
        "base" => WordFormTypeV3::Base,
        "third_person_singular" => WordFormTypeV3::ThirdPersonSingular,
        "present_participle" => WordFormTypeV3::PresentParticiple,
        "past_tense" => WordFormTypeV3::PastTense,
        "past_participle" => WordFormTypeV3::PastParticiple,
        "plural" => WordFormTypeV3::Plural,
        "comparative" => WordFormTypeV3::Comparative,
        "superlative" => WordFormTypeV3::Superlative,
        _ => return None,
    })
}

fn v3_form_type_name(form_type: crate::lexicon::dto::WordFormTypeV3) -> &'static str {
    use crate::lexicon::dto::WordFormTypeV3;

    match form_type {
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

fn manual_target(
    target: &PublishedAssociationTarget,
    sense_id: Uuid,
    normalized_surface: &str,
    target_publication_id: Option<Uuid>,
    target_form_variant_id: Option<Uuid>,
) -> Option<ResolvedTarget> {
    target.manual_target(
        sense_id,
        normalized_surface,
        target_publication_id,
        target_form_variant_id,
    )
}

impl LexiconService {
    /// 读取路径回填：`associations` 不进编辑器投影，也不进发布快照，只有库表一份真相，
    /// 返回词条时按 `entry_id` 一次查出挂回去。
    ///
    /// 写路径必须**在写幂等响应体之前**调用，否则同一个幂等键重放时返回的响应会缺字段。
    /// 新建草稿（必然未解析）与归档/恢复批量命令不回填：前者默认值就是对的，
    /// 后者不碰例句，响应也不用于渲染例句编辑区。
    pub(super) async fn hydrate_sentence_associations(
        &self,
        word: &mut AdminWordV2,
    ) -> Result<(), LexiconServiceError> {
        let associations =
            LexiconRepository::sentence_associations(self.repository.pool(), word.id)
                .await
                .map_err(repository_error)?;
        let scans = LexiconRepository::sentence_association_scans(self.repository.pool(), word.id)
            .await
            .map_err(repository_error)?;
        apply_sentence_associations(&mut word.meanings, associations, scans);
        Ok(())
    }

    pub(super) async fn hydrate_sentence_associations_in(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        word: &mut AdminWordV2,
    ) -> Result<(), LexiconServiceError> {
        let associations = LexiconRepository::sentence_associations(&mut **tx, word.id)
            .await
            .map_err(repository_error)?;
        let scans = LexiconRepository::sentence_association_scans(&mut **tx, word.id)
            .await
            .map_err(repository_error)?;
        apply_sentence_associations(&mut word.meanings, associations, scans);
        Ok(())
    }

    pub(super) async fn hydrate_v3_sentence_associations_in(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        entry_id: Uuid,
        meanings: &mut crate::lexicon::dto::DraftMeaningsStepContentV3,
    ) -> Result<(), LexiconServiceError> {
        let associations = LexiconRepository::sentence_associations(&mut **tx, entry_id)
            .await
            .map_err(repository_error)?;
        let scans = LexiconRepository::sentence_association_scans(&mut **tx, entry_id)
            .await
            .map_err(repository_error)?;
        apply_sentence_associations_v3(meanings, associations, scans);
        Ok(())
    }

    pub(super) async fn hydrate_v3_sentence_associations(
        &self,
        entry_id: Uuid,
        meanings: &mut crate::lexicon::dto::DraftMeaningsStepContentV3,
    ) -> Result<(), LexiconServiceError> {
        let associations =
            LexiconRepository::sentence_associations(self.repository.pool(), entry_id)
                .await
                .map_err(repository_error)?;
        let scans = LexiconRepository::sentence_association_scans(self.repository.pool(), entry_id)
            .await
            .map_err(repository_error)?;
        apply_sentence_associations_v3(meanings, associations, scans);
        Ok(())
    }

    /// 发布时重新解析本词条的例句关联。
    ///
    /// 失败语义与关联词物化相反：解析不出目标、有歧义、词面不合法都只是「这个词不关联」，
    /// **不产生任何 issue，发布照常成功**。只有数据库故障会让整个事务回滚。
    ///
    /// 正文指纹没变的那一侧原地不动，因此管理员事后修正过的关联能活过下一次发布；
    /// 变了的那一侧整侧重算。
    pub(super) async fn refresh_sentence_associations(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        entry_id: Uuid,
        meanings: &DraftMeaningsStepContent,
        allow_v3_targets: bool,
        allow_automatic_associations: bool,
        only_sentence_id: Option<Uuid>,
    ) -> Result<(), LexiconServiceError> {
        let variants = sentence_variants(meanings)
            .into_iter()
            .filter(|variant| only_sentence_id.is_none_or(|id| variant.sentence_id == id))
            .collect::<Vec<_>>();
        let live_sentence_ids = variants
            .iter()
            .map(|variant| variant.sentence_id)
            .collect::<Vec<_>>();
        let live_dialects = variants
            .iter()
            .map(|variant| dialect_name(variant.dialect).to_owned())
            .collect::<Vec<_>>();
        if only_sentence_id.is_none() {
            LexiconRepository::prune_sentence_associations(
                tx,
                entry_id,
                &live_sentence_ids,
                &live_dialects,
            )
            .await
            .map_err(repository_error)?;
        }

        let scanned = LexiconRepository::sentence_association_scans(&mut **tx, entry_id)
            .await
            .map_err(repository_error)?
            .into_iter()
            .map(|scan| {
                (
                    (scan.sentence_id, scan.source_dialect),
                    (scan.text_hash, scan.resolver_version),
                )
            })
            .collect::<HashMap<_, _>>();

        let pending = variants
            .into_iter()
            .filter_map(|variant| {
                let hash = text_hash(variant.text);
                let key = (
                    variant.sentence_id,
                    dialect_name(variant.dialect).to_owned(),
                );
                let previous = scanned.get(&key);
                let unchanged_text = previous
                    .map(|(text_hash, _)| text_hash == &hash)
                    .unwrap_or(false);
                let current = previous
                    .map(|(text_hash, resolver_version)| {
                        text_hash == &hash && *resolver_version == RESOLVER_VERSION
                    })
                    .unwrap_or(false);
                (!current).then_some((variant, hash, unchanged_text))
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }

        let mut preserved_manual = LexiconRepository::sentence_associations(&mut **tx, entry_id)
            .await
            .map_err(repository_error)?
            .into_iter()
            .filter(|association| association.origin == "manual")
            .fold(HashMap::new(), |mut grouped, association| {
                grouped
                    .entry((association.sentence_id, association.source_dialect.clone()))
                    .or_insert_with(Vec::new)
                    .push(association);
                grouped
            });

        if !allow_automatic_associations {
            for (variant, hash, unchanged_text) in pending {
                let dialect = dialect_name(variant.dialect);
                let associations = if unchanged_text {
                    preserved_manual
                        .remove(&(variant.sentence_id, dialect.to_owned()))
                        .unwrap_or_default()
                        .into_iter()
                        .map(new_association_from_record)
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                LexiconRepository::replace_sentence_associations(
                    tx,
                    entry_id,
                    variant.sentence_id,
                    dialect,
                    &associations,
                    Some((&hash, RESOLVER_VERSION)),
                )
                .await
                .map_err(repository_error)?;
            }
            return Ok(());
        }

        let tokenized = pending
            .iter()
            .map(|(variant, _, _)| {
                tokenize(variant.text)
                    .into_iter()
                    .filter(|token| !is_stopword(&token.normalized))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let mut scopes = pending
            .iter()
            .flat_map(|(variant, _, _)| lookup_scopes(variant.dialect))
            .map(|scope| (*scope).to_owned())
            .collect::<Vec<_>>();
        scopes.sort_unstable();
        scopes.dedup();
        let mut surfaces = tokenized
            .iter()
            .flatten()
            .map(|token| token.normalized.clone())
            .collect::<Vec<_>>();
        surfaces.sort_unstable();
        surfaces.dedup();

        let candidates = LexiconRepository::published_form_surfaces(
            tx,
            entry_id,
            &scopes,
            &surfaces,
            allow_v3_targets,
        )
        .await
        .map_err(repository_error)?
        .into_iter()
        .filter(|candidate| associable_pos(&candidate.pos))
        .collect::<Vec<_>>();
        let mut target_entry_ids = candidates
            .iter()
            .map(|candidate| candidate.entry_id)
            .collect::<Vec<_>>();
        target_entry_ids.sort_unstable();
        target_entry_ids.dedup();
        let snapshots = LexiconRepository::current_publication_snapshots(tx, &target_entry_ids)
            .await
            .map_err(repository_error)?
            .into_iter()
            .map(|record| {
                PublishedAssociationTarget::from_snapshot(
                    record.snapshot,
                    allow_v3_targets,
                    record.publication_id,
                )
                .map(|target| (record.entry_id, target))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;

        for ((variant, hash, unchanged_text), tokens) in pending.into_iter().zip(tokenized) {
            let dialect = dialect_name(variant.dialect);
            let scopes = lookup_scopes(variant.dialect);
            let mut associations = tokens
                .iter()
                .filter_map(|token| {
                    let target = resolve_token(token, scopes, &candidates, &snapshots)?;
                    Some(NewSentenceAssociation {
                        id: Uuid::now_v7(),
                        sentence_id: variant.sentence_id,
                        source_dialect: dialect.to_owned(),
                        association_schema_version: 2,
                        source_segments: vec![NewSentenceAssociationSegment {
                            range_start: i32::try_from(token.start).ok()?,
                            range_end: i32::try_from(token.end).ok()?,
                            surface: token.surface.clone(),
                        }],
                        segments_fingerprint: None,
                        range_start: i32::try_from(token.start).ok()?,
                        range_end: i32::try_from(token.end).ok()?,
                        surface: token.surface.clone(),
                        state: "linked".to_owned(),
                        target_entry_id: Some(target.target_entry_id),
                        target_sense_id: Some(target.target_sense_id),
                        target_form_slot_id: target.target_form_slot_id,
                        target_publication_id: None,
                        target_form_variant_id: None,
                        target_component_usages_snapshot: None,
                        origin: "auto".to_owned(),
                        target_headword_snapshot: Some(target.target_headword),
                        target_gloss_snapshot: Some(target.target_gloss),
                        resolved_pos: Some(target.resolved_pos),
                        resolved_form_type: target.resolved_form_type,
                        pending_target_kind: None,
                        pending_target_headword: None,
                        normalized_pending_target_headword: None,
                        pending_target_gloss: None,
                    })
                })
                .collect::<Vec<_>>();
            if unchanged_text {
                let manual = preserved_manual
                    .remove(&(variant.sentence_id, dialect.to_owned()))
                    .unwrap_or_default();
                associations.retain(|automatic| {
                    manual.iter().all(|existing| {
                        automatic.range_end <= existing.range_start
                            || automatic.range_start >= existing.range_end
                    })
                });
                associations.extend(manual.into_iter().map(new_association_from_record));
                associations.sort_by_key(|association| association.range_start);
            }
            LexiconRepository::replace_sentence_associations(
                tx,
                entry_id,
                variant.sentence_id,
                dialect,
                &associations,
                Some((&hash, RESOLVER_VERSION)),
            )
            .await
            .map_err(repository_error)?;
        }
        Ok(())
    }
}

/// 一个候选词面能不能落成关联：词条、词性、词义三层都唯一才行，任一层不唯一就跳过。
///
/// 错误关联比缺失关联难被发现——缺失是空白，管理员看一眼就知道要补；错关到别的义项
/// 在界面上和正确关联长得一模一样。库里现有的排序信号都不是「这句话里用的是哪个义项」
/// 的证据，拿它们择一等于给猜测背书。
fn resolve_token(
    token: &SentenceToken,
    scopes: &[&str],
    candidates: &[PublishedFormSurfaceRecord],
    snapshots: &HashMap<Uuid, PublishedAssociationTarget>,
) -> Option<ResolvedTarget> {
    let mut matched = candidates
        .iter()
        .filter(|candidate| {
            candidate.normalized_surface == token.normalized
                && scopes.contains(&candidate.dialect_scope.as_str())
        })
        .map(|candidate| {
            (
                candidate.entry_id,
                candidate.pos_id,
                candidate.source_node_id,
            )
        })
        .collect::<Vec<_>>();
    matched.sort_unstable();
    matched.dedup();

    let (entry_id, pos_id, _) = matched.first().copied()?;
    if matched
        .iter()
        .any(|(other_entry, other_pos, _)| *other_entry != entry_id || *other_pos != pos_id)
    {
        return None;
    }

    // 同一个词性下可能有多个槽位共用一个拼写——不规则动词 cut 的原形、过去式、
    // 过去分词都是 cut。词义仍然唯一，所以关联成立；但槽位是哪一个没有证据，
    // 按变体 ID 的大小随手挑一个等于给猜测背书，这里留空，前端回落到原句词面。
    // 按槽位 ID 去重要用有序集合：matched 是按变体 ID 排的，同一槽位的两个方言变体
    // 之间可能夹着别的槽位，Vec::dedup_by 只并相邻项会漏掉。
    let variant_ids = matched
        .iter()
        .map(|(_, _, variant_id)| *variant_id)
        .collect::<Vec<_>>();
    snapshots
        .get(&entry_id)?
        .automatic_target(pos_id, &variant_ids)
}

impl LexiconService {
    /// 事后修正一条例句的关联：整组替换，覆盖这条例句所有存在的方言侧。
    ///
    /// 人工整组保存会针对当前正文重新校验所有 range/surface，并写入对应 scan；因此它既
    /// 能修正已解析例句，也能作为新增例句两阶段保存的第二步把 unresolved 建立为 resolved。
    ///
    /// 推进的是 `lifecycle_revision` 而不是 `revision`：修正的是已发布内容的附属数据，
    /// 不该把词条判成「有未发布改动」再逼一次重新发布。
    #[allow(clippy::too_many_arguments)]
    pub async fn replace_sentence_associations(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        sentence_id: Uuid,
        idempotency_key: Uuid,
        input: ReplaceSentenceAssociationsInput,
        allow_v3: bool,
        allow_pending: bool,
        allow_automatic_associations: bool,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        if input.base_revision() < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_revision",
                message: "base_revision must be at least 1",
            });
        }
        if input.base_lifecycle_revision() < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_lifecycle_revision",
                message: "base_lifecycle_revision must be at least 1",
            });
        }
        let request_hash = sha256_json(&serde_json::json!({
            "entry_id": entry_id,
            "sentence_id": sentence_id,
            "input": input,
        }))
        .map_err(serialization_error)?;
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "{REPLACE_ASSOCIATIONS_SCOPE}:{actor_id}:{idempotency_key}"
            ))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) = LexiconRepository::idempotency(
            &mut transaction,
            REPLACE_ASSOCIATIONS_SCOPE,
            actor_id,
            idempotency_key,
        )
        .await
        .map_err(repository_error)?
        {
            if existing.request_hash != request_hash {
                return Err(LexiconServiceError::IdempotencyConflict);
            }
            let response: AdminWordAnyEnvelope =
                serde_json::from_value(existing.response_body).map_err(serialization_error)?;
            ensure_association_response_capability(&response, allow_v3)?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(response);
        }

        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        let record_revision = record.revision;
        let record_lifecycle_revision = record.lifecycle_revision;
        if !input.is_v3()
            && LexiconRepository::sentence_has_segmented_associations(&mut transaction, sentence_id)
                .await
                .map_err(repository_error)?
        {
            return Err(LexiconServiceError::SentenceAssociationClientUpgradeRequired);
        }
        let mut word = match record.content_schema_version {
            2 => AdminWordAny::V2(Box::new(entry_from_record(record)?)),
            3 if allow_v3 => {
                let word = self.get_v3(entry_id).await?;
                if word.revision != record_revision
                    || word.lifecycle_revision != record_lifecycle_revision
                {
                    return Err(invariant_record());
                }
                AdminWordAny::V3(Box::new(word))
            }
            3 => return Err(LexiconServiceError::V3StorageUnavailable),
            version => return Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
        };
        ensure_association_source_state(
            &word,
            input.base_revision(),
            input.base_lifecycle_revision(),
        )?;
        if input.is_v3() && !matches!(word, AdminWordAny::V3(_)) {
            return Err(LexiconServiceError::ValidationFailed(vec![
                reference_issue(
                    sentence_id,
                    "associations",
                    "sentence_association_v3_requires_v3_entry",
                    "多片段例句关联只允许写入 V3 词条",
                ),
            ]));
        }
        let normalized_inputs = normalize_manual_association_inputs(&input);
        let has_pending = normalized_inputs
            .iter()
            .any(|association| association.pending_target_kind.is_some());
        if has_pending && !allow_pending {
            return Err(LexiconServiceError::V3StorageUnavailable);
        }
        if matches!(word, AdminWordAny::V2(_)) && has_pending {
            return Err(LexiconServiceError::ValidationFailed(vec![
                reference_issue(
                    sentence_id,
                    "associations",
                    "sentence_association_pending_requires_v3",
                    "Pending 例句关联只允许由 V3 词条创建",
                ),
            ]));
        }

        let meanings = association_source_meanings(&word)?;
        let sentence = meanings
            .pos
            .iter()
            .flat_map(|pos| &pos.senses)
            .flat_map(|sense| &sense.sentences)
            .find(|sentence| sentence.id == sentence_id)
            .ok_or(LexiconServiceError::SentenceNotFound)?;
        let variants = present_variants(&sentence.en_text)
            .into_iter()
            .map(|(dialect, text)| (dialect, text.to_owned()))
            .collect::<Vec<_>>();
        let scans = LexiconRepository::sentence_association_scans(&mut *transaction, entry_id)
            .await
            .map_err(repository_error)?;
        let was_resolved = !variants.is_empty()
            && variants.iter().all(|(dialect, text)| {
                scans.iter().any(|scan| {
                    scan.sentence_id == sentence_id
                        && scan.source_dialect == dialect_name(*dialect)
                        && scan.resolver_version == RESOLVER_VERSION
                        && scan.text_hash == text_hash(text)
                })
            });
        if !was_resolved {
            Self::refresh_sentence_associations(
                &mut transaction,
                entry_id,
                &meanings,
                allow_v3,
                allow_automatic_associations,
                Some(sentence_id),
            )
            .await?;
        }
        let existing = LexiconRepository::sentence_associations(&mut *transaction, entry_id)
            .await
            .map_err(repository_error)?
            .into_iter()
            .filter(|association| association.sentence_id == sentence_id)
            .map(|association| (association.id, association))
            .collect::<HashMap<_, _>>();

        let mut target_ids = normalized_inputs
            .iter()
            .filter(|association| association.target_publication_id.is_none())
            .filter_map(|association| association.target_word_id)
            .filter(|target_id| *target_id != entry_id)
            .collect::<Vec<_>>();
        target_ids.sort_unstable();
        target_ids.dedup();
        let mut historical_targets = normalized_inputs
            .iter()
            .filter_map(|association| {
                Some((
                    association.target_word_id?,
                    association.target_publication_id?,
                ))
            })
            .filter(|(target_id, _)| *target_id != entry_id)
            .collect::<Vec<_>>();
        historical_targets.sort_unstable();
        historical_targets.dedup();
        let current_records =
            LexiconRepository::current_publication_snapshots(&mut transaction, &target_ids)
                .await
                .map_err(repository_error)?;
        let historical_records = LexiconRepository::historical_publication_snapshots(
            &mut transaction,
            &historical_targets,
        )
        .await
        .map_err(repository_error)?;
        let targets = current_records
            .into_iter()
            .map(|record| (record, None))
            .chain(historical_records.into_iter().map(|record| {
                let publication_id = record.publication_id;
                (record, Some(publication_id))
            }))
            .map(|(record, requested_publication_id)| {
                PublishedAssociationTarget::from_snapshot(
                    record.snapshot,
                    allow_v3,
                    record.publication_id,
                )
                .map(|target| ((record.entry_id, requested_publication_id), target))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;

        let (rows, issues) = validate_manual_associations(
            entry_id,
            sentence_id,
            &variants,
            &normalized_inputs,
            &existing,
            &targets,
        );
        if !issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(issues));
        }

        for (dialect, text) in &variants {
            let dialect = dialect_name(*dialect);
            let hash = text_hash(text);
            let mut side = rows
                .iter()
                .filter(|row| row.source_dialect == dialect)
                .cloned()
                .collect::<Vec<_>>();
            if !was_resolved {
                let automatic = existing
                    .values()
                    .filter(|association| {
                        association.origin == "auto" && association.source_dialect == dialect
                    })
                    .cloned()
                    .map(new_association_from_record)
                    .filter(|association| {
                        side.iter().all(|manual| {
                            !segments_overlap(&association.source_segments, &manual.source_segments)
                        })
                    })
                    .collect::<Vec<_>>();
                side.extend(automatic);
                side.sort_by_key(|association| association.range_start);
            }
            LexiconRepository::replace_sentence_associations(
                &mut transaction,
                entry_id,
                sentence_id,
                dialect,
                &side,
                Some((&hash, RESOLVER_VERSION)),
            )
            .await
            .map_err(repository_error)?;
        }

        let updated_at = Utc::now();
        let (revision, lifecycle_revision) = match &mut word {
            AdminWordAny::V2(word) => {
                word.lifecycle_revision += 1;
                word.updated_at = updated_at;
                (word.revision, word.lifecycle_revision)
            }
            AdminWordAny::V3(word) => {
                word.lifecycle_revision += 1;
                word.updated_at = updated_at;
                (word.revision, word.lifecycle_revision)
            }
        };
        LexiconRepository::record_sentence_association_edit(
            &mut transaction,
            entry_id,
            revision,
            lifecycle_revision,
            updated_at,
            actor_id,
            request_id,
            sentence_id,
            "lexicon.sentence_associations.replace",
        )
        .await
        .map_err(repository_error)?;
        match &mut word {
            AdminWordAny::V2(word) => {
                Self::hydrate_sentence_associations_in(&mut transaction, word).await?
            }
            AdminWordAny::V3(word) => {
                Self::hydrate_v3_sentence_associations_in(
                    &mut transaction,
                    word.id,
                    &mut word.meanings,
                )
                .await?
            }
        }
        let response = AdminWordAnyEnvelope { word };
        LexiconRepository::insert_idempotent_response(
            &mut transaction,
            REPLACE_ASSOCIATIONS_SCOPE,
            actor_id,
            idempotency_key,
            &request_hash,
            Some(sentence_id),
            &response,
            200,
        )
        .await
        .map_err(repository_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(response)
    }

    pub async fn pending_sentence_associations(
        &self,
        target_entry_id: Uuid,
        query: PendingSentenceAssociationListQuery,
        allow_v3: bool,
    ) -> Result<PendingSentenceAssociationListResponse, LexiconServiceError> {
        if !allow_v3 {
            return Err(LexiconServiceError::V3StorageUnavailable);
        }
        let page_size = query.page_size.unwrap_or(20);
        if !(1..=100).contains(&page_size) {
            return Err(LexiconServiceError::InvalidField {
                field: "page_size",
                message: "page_size must be between 1 and 100",
            });
        }
        let target_kind = LexiconRepository::published_sentence_association_target_kind(
            self.repository.pool(),
            target_entry_id,
        )
        .await
        .map_err(repository_error)?
        .ok_or(LexiconServiceError::WordNotFound)?;
        if target_kind != "word" && target_kind != "phrase" {
            return Err(LexiconServiceError::WordNotFound);
        }
        let scan_limit = i64::from(page_size) * 4 + 1;
        let mut records = LexiconRepository::pending_sentence_associations_for_target(
            self.repository.pool(),
            target_entry_id,
            query.cursor,
            scan_limit,
        )
        .await
        .map_err(repository_error)?;
        let scan_has_more = records.len() == scan_limit as usize;
        let last_scanned_id = records.last().map(|record| record.id);
        records.retain(|record| {
            record.scan_resolver_version == RESOLVER_VERSION
                && record.scan_text_hash == text_hash(&record.sentence_text)
        });
        let total = LexiconRepository::pending_sentence_association_count_for_target(
            self.repository.pool(),
            target_entry_id,
            RESOLVER_VERSION,
        )
        .await
        .map_err(repository_error)? as u64;
        let has_more = records.len() > page_size as usize || scan_has_more;
        records.truncate(page_size as usize);
        let next_cursor = has_more
            .then(|| {
                records
                    .last()
                    .map(|record| record.id)
                    .or(last_scanned_id)
                    .map(|id| id.to_string())
            })
            .flatten();
        let results = records
            .into_iter()
            .map(pending_sentence_association_item)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PendingSentenceAssociationListResponse {
            results,
            total,
            next_cursor,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn claim_pending_sentence_association(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        association_id: Uuid,
        idempotency_key: Uuid,
        input: ClaimPendingSentenceAssociationInput,
        allow_v3: bool,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        if !allow_v3 {
            return Err(LexiconServiceError::V3StorageUnavailable);
        }
        if input.base_owner_entry_revision < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_owner_entry_revision",
                message: "base_owner_entry_revision must be at least 1",
            });
        }
        if input.base_owner_lifecycle_revision < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_owner_lifecycle_revision",
                message: "base_owner_lifecycle_revision must be at least 1",
            });
        }
        let request_hash = sha256_json(&serde_json::json!({
            "association_id": association_id,
            "input": input,
        }))
        .map_err(serialization_error)?;
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "{CLAIM_PENDING_ASSOCIATION_SCOPE}:{actor_id}:{idempotency_key}"
            ))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) = LexiconRepository::idempotency(
            &mut transaction,
            CLAIM_PENDING_ASSOCIATION_SCOPE,
            actor_id,
            idempotency_key,
        )
        .await
        .map_err(repository_error)?
        {
            if existing.request_hash != request_hash {
                return Err(LexiconServiceError::IdempotencyConflict);
            }
            let response: AdminWordAnyEnvelope =
                serde_json::from_value(existing.response_body).map_err(serialization_error)?;
            ensure_association_response_capability(&response, allow_v3)?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(response);
        }

        let owner_entry_id =
            LexiconRepository::sentence_association_owner_id(&mut transaction, association_id)
                .await
                .map_err(repository_error)?
                .ok_or(LexiconServiceError::PendingSentenceAssociationNotFound)?;
        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, owner_entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        let record_revision = record.revision;
        let record_lifecycle_revision = record.lifecycle_revision;
        let mut word = match record.content_schema_version {
            2 => return Err(LexiconServiceError::PendingSentenceAssociationNotFound),
            3 => {
                let word = self.get_v3(owner_entry_id).await?;
                if word.revision != record_revision
                    || word.lifecycle_revision != record_lifecycle_revision
                {
                    return Err(invariant_record());
                }
                AdminWordAny::V3(Box::new(word))
            }
            version => return Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
        };
        let association = LexiconRepository::sentence_association_by_id_for_update(
            &mut transaction,
            association_id,
        )
        .await
        .map_err(repository_error)?
        .ok_or(LexiconServiceError::PendingSentenceAssociationNotFound)?;
        if association.entry_id != owner_entry_id {
            return Err(LexiconServiceError::PendingSentenceAssociationNotFound);
        }
        if association.state != "pending" {
            return Err(LexiconServiceError::PendingSentenceAssociationClaimed);
        }
        ensure_association_source_state(
            &word,
            input.base_owner_entry_revision,
            input.base_owner_lifecycle_revision,
        )?;
        let source_text = LexiconRepository::sentence_association_current_text(
            &mut transaction,
            owner_entry_id,
            association.sentence_id,
            &association.source_dialect,
        )
        .await
        .map_err(repository_error)?
        .ok_or(LexiconServiceError::PendingSentenceAssociationNotFound)?;
        let active_scan =
            LexiconRepository::sentence_association_scans(&mut *transaction, owner_entry_id)
                .await
                .map_err(repository_error)?
                .into_iter()
                .find(|scan| {
                    scan.sentence_id == association.sentence_id
                        && scan.source_dialect == association.source_dialect
                });
        let source_segments = serde_json::from_value::<Vec<SentenceSourceRangeV1>>(
            association.source_segments.clone(),
        )
        .map_err(serialization_error)?;
        let active_range = source_segments.len()
            == usize::try_from(association.segment_count).unwrap_or_default()
            && !source_segments.is_empty()
            && source_segments.iter().all(|segment| {
                codepoint_slice(&source_text, segment.start, segment.end)
                    .is_some_and(|surface| surface == segment.surface)
            });
        let active_scan = active_scan.is_some_and(|scan| {
            scan.resolver_version == RESOLVER_VERSION && scan.text_hash == text_hash(&source_text)
        });
        if !active_scan || !active_range {
            return Err(LexiconServiceError::PendingSentenceAssociationNotFound);
        }
        if input.target_word_id == owner_entry_id {
            return Err(LexiconServiceError::ValidationFailed(vec![
                reference_issue(
                    association.sentence_id,
                    "associations",
                    "sentence_association_self_target",
                    "例句关联不能指向当前词条自身",
                ),
            ]));
        }
        let snapshot = LexiconRepository::current_publication_snapshots(
            &mut transaction,
            &[input.target_word_id],
        )
        .await
        .map_err(repository_error)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            LexiconServiceError::ValidationFailed(vec![reference_issue(
                association.sentence_id,
                "associations",
                "sentence_association_target_unavailable",
                "认领目标必须是未归档词条当前发布版本中的有效词义",
            )])
        })?;
        let target = PublishedAssociationTarget::from_snapshot(
            snapshot.snapshot,
            allow_v3,
            snapshot.publication_id,
        )?;
        let pending_kind = association.pending_target_kind.as_deref();
        let target_kind = match target.kind {
            EntryKind::Word => "word",
            EntryKind::Phrase => "phrase",
        };
        if pending_kind != Some(target_kind) {
            return Err(LexiconServiceError::ValidationFailed(vec![
                reference_issue(
                    association.sentence_id,
                    "associations",
                    "sentence_association_target_kind_mismatch",
                    "认领目标种类必须与 Pending 的 word/phrase 种类一致",
                ),
            ]));
        }
        let normalized = association
            .normalized_pending_target_headword
            .as_deref()
            .unwrap_or_default();
        let target_surface_matches = normalized_surface(&target.headword).as_deref()
            == Some(normalized)
            || target.pos.iter().flat_map(|pos| &pos.forms).any(|form| {
                form.normalized_surfaces
                    .iter()
                    .any(|surface| surface == normalized)
            });
        if !target_surface_matches {
            return Err(LexiconServiceError::ValidationFailed(vec![
                reference_issue(
                    association.sentence_id,
                    "associations",
                    "sentence_association_target_surface_mismatch",
                    "认领目标的当前发布词面必须与 Pending 词面一致",
                ),
            ]));
        }
        let resolved = manual_target(
            &target,
            input.target_sense_id,
            normalized,
            input
                .target_publication_id
                .or(Some(snapshot.publication_id)),
            input.target_form_variant_id,
        )
        .ok_or_else(|| {
            LexiconServiceError::ValidationFailed(vec![reference_issue(
                association.sentence_id,
                "associations",
                "sentence_association_target_unavailable",
                "认领目标必须包含可用的具体词义",
            )])
        })?;
        let target_component_usages_snapshot = resolved
            .target_form_variant_id
            .map(|_| serde_json::to_value(&resolved.target_component_usages))
            .transpose()
            .map_err(serialization_error)?;
        LexiconRepository::claim_pending_sentence_association(
            &mut transaction,
            association_id,
            resolved.target_entry_id,
            resolved.target_sense_id,
            resolved.target_form_slot_id,
            resolved.target_publication_id,
            resolved.target_form_variant_id,
            target_component_usages_snapshot.as_ref(),
            &resolved.target_headword,
            &resolved.target_gloss,
            &resolved.resolved_pos,
            resolved.resolved_form_type.as_deref(),
        )
        .await
        .map_err(repository_error)?;

        let updated_at = Utc::now();
        let (revision, lifecycle_revision) = match &mut word {
            AdminWordAny::V2(word) => {
                word.lifecycle_revision += 1;
                word.updated_at = updated_at;
                (word.revision, word.lifecycle_revision)
            }
            AdminWordAny::V3(word) => {
                word.lifecycle_revision += 1;
                word.updated_at = updated_at;
                (word.revision, word.lifecycle_revision)
            }
        };
        LexiconRepository::record_sentence_association_edit(
            &mut transaction,
            owner_entry_id,
            revision,
            lifecycle_revision,
            updated_at,
            actor_id,
            request_id,
            association.sentence_id,
            "lexicon.sentence_associations.claim",
        )
        .await
        .map_err(repository_error)?;
        match &mut word {
            AdminWordAny::V2(word) => {
                Self::hydrate_sentence_associations_in(&mut transaction, word).await?
            }
            AdminWordAny::V3(word) => {
                Self::hydrate_v3_sentence_associations_in(
                    &mut transaction,
                    word.id,
                    &mut word.meanings,
                )
                .await?
            }
        }
        let response = AdminWordAnyEnvelope { word };
        LexiconRepository::insert_idempotent_response(
            &mut transaction,
            CLAIM_PENDING_ASSOCIATION_SCOPE,
            actor_id,
            idempotency_key,
            &request_hash,
            Some(association_id),
            &response,
            200,
        )
        .await
        .map_err(repository_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(response)
    }
}

fn pending_sentence_association_item(
    record: PendingSentenceAssociationListRecord,
) -> Result<PendingSentenceAssociationItemV3, LexiconServiceError> {
    let source_dialect = dialect_from_name(&record.source_dialect).ok_or_else(|| {
        LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
            "pending sentence association has invalid source dialect",
        ))
    })?;
    let pending_target_kind = match record.pending_target_kind.as_str() {
        "word" => EntryKind::Word,
        "phrase" => EntryKind::Phrase,
        _ => {
            return Err(LexiconServiceError::Repository(
                LexiconRepositoryError::Invariant(
                    "pending sentence association has invalid target kind",
                ),
            ));
        }
    };
    if !matches!(record.association_schema_version, 2 | 3) {
        return Err(invariant_record());
    }
    let source_segments =
        serde_json::from_value::<Vec<SentenceSourceRangeV1>>(record.source_segments)
            .map_err(serialization_error)?;
    if source_segments.is_empty()
        || source_segments.len() != usize::try_from(record.segment_count).unwrap_or_default()
    {
        return Err(invariant_record());
    }
    Ok(PendingSentenceAssociationItemV3 {
        association_id: record.id,
        owner_entry_id: record.entry_id,
        owner_entry_revision: record.owner_revision,
        owner_lifecycle_revision: record.owner_lifecycle_revision,
        sentence_id: record.sentence_id,
        source_dialect,
        source_segments,
        sentence_text: record.sentence_text,
        pending_target_kind,
        pending_target_headword: record.pending_target_headword,
        pending_target_gloss: record.pending_target_gloss,
    })
}

fn ensure_association_source_state(
    word: &AdminWordAny,
    base_revision: i64,
    base_lifecycle_revision: i64,
) -> Result<(), LexiconServiceError> {
    let (revision, lifecycle_revision, archived) = match word {
        AdminWordAny::V2(word) => (
            word.revision,
            word.lifecycle_revision,
            word.archived_at.is_some(),
        ),
        AdminWordAny::V3(word) => (
            word.revision,
            word.lifecycle_revision,
            word.archived_at.is_some(),
        ),
    };
    if archived {
        return Err(LexiconServiceError::EntryArchived);
    }
    if revision != base_revision {
        return Err(LexiconServiceError::RevisionConflict {
            current_revision: revision,
        });
    }
    if lifecycle_revision != base_lifecycle_revision {
        return Err(LexiconServiceError::LifecycleRevisionConflict {
            current_lifecycle_revision: lifecycle_revision,
        });
    }
    Ok(())
}

fn ensure_association_response_capability(
    response: &AdminWordAnyEnvelope,
    allow_v3: bool,
) -> Result<(), LexiconServiceError> {
    if !allow_v3 && matches!(&response.word, AdminWordAny::V3(_)) {
        return Err(LexiconServiceError::V3StorageUnavailable);
    }
    Ok(())
}

fn association_source_meanings(
    word: &AdminWordAny,
) -> Result<DraftMeaningsStepContent, LexiconServiceError> {
    match word {
        AdminWordAny::V2(word) => Ok(word.meanings.clone()),
        AdminWordAny::V3(word) => {
            let mut meanings = word.meanings.clone();
            for sentence in meanings
                .pos
                .iter_mut()
                .flat_map(|pos| &mut pos.senses)
                .flat_map(|sense| &mut sense.sentences)
            {
                sentence.associations.clear();
                sentence.associations_state = SentenceAssociationsStateV2::Unresolved;
            }
            serde_json::from_value(serde_json::to_value(meanings).map_err(serialization_error)?)
                .map_err(serialization_error)
        }
    }
}

#[derive(Debug, Clone)]
struct ManualAssociationInput {
    id: Uuid,
    source_dialect: Dialect,
    association_schema_version: i16,
    source_segments: Vec<SentenceSourceRangeV1>,
    target_word_id: Option<Uuid>,
    target_sense_id: Option<Uuid>,
    target_publication_id: Option<Uuid>,
    target_form_variant_id: Option<Uuid>,
    pending_target_kind: Option<EntryKind>,
    pending_target_headword: Option<String>,
    pending_target_gloss: Option<String>,
}

fn normalize_manual_association_inputs(
    input: &ReplaceSentenceAssociationsInput,
) -> Vec<ManualAssociationInput> {
    match input {
        ReplaceSentenceAssociationsInput::V2(input) => input
            .associations
            .iter()
            .map(|association| ManualAssociationInput {
                id: association.id,
                source_dialect: association.source_dialect,
                association_schema_version: 2,
                source_segments: vec![association.source_range.clone()],
                target_word_id: association.target_word_id,
                target_sense_id: association.target_sense_id,
                target_publication_id: None,
                target_form_variant_id: None,
                pending_target_kind: association.pending_target_kind,
                pending_target_headword: association.pending_target_headword.clone(),
                pending_target_gloss: association.pending_target_gloss.clone(),
            })
            .collect(),
        ReplaceSentenceAssociationsInput::V3(input) => input
            .associations
            .iter()
            .map(|association| ManualAssociationInput {
                id: association.id,
                source_dialect: association.source_dialect,
                association_schema_version: 3,
                source_segments: association.source_segments.clone(),
                target_word_id: association.target_word_id,
                target_sense_id: association.target_sense_id,
                target_publication_id: association.target_publication_id,
                target_form_variant_id: association.target_form_variant_id,
                pending_target_kind: association.pending_target_kind,
                pending_target_headword: association.pending_target_headword.clone(),
                pending_target_gloss: association.pending_target_gloss.clone(),
            })
            .collect(),
    }
}

fn segments_overlap(
    left: &[NewSentenceAssociationSegment],
    right: &[NewSentenceAssociationSegment],
) -> bool {
    let as_discovery_segments = |segments: &[NewSentenceAssociationSegment]| {
        segments
            .iter()
            .filter_map(|segment| {
                Some(DiscoverySourceSegment {
                    range: DiscoveryCodepointRange {
                        start: usize::try_from(segment.range_start).ok()?,
                        end: usize::try_from(segment.range_end).ok()?,
                    },
                })
            })
            .collect::<Vec<_>>()
    };
    any_segment_intersection(&as_discovery_segments(left), &as_discovery_segments(right))
}

fn segments_fingerprint(
    sentence_id: Uuid,
    dialect: &str,
    text: &str,
    segments: &[NewSentenceAssociationSegment],
) -> Vec<u8> {
    let positions = segments
        .iter()
        .map(|segment| format!("{}:{}", segment.range_start, segment.range_end))
        .collect::<Vec<_>>()
        .join("|");
    text_hash(&format!(
        "3|{sentence_id}|{dialect}|{}|{}|{positions}",
        format_args!("{:x?}", text_hash(text)),
        segments.len()
    ))
}

/// 逐条校验人工关联，顺带把只读投影解析出来。V3 的位置权威始终是
/// `source_segments`；parent 上的 legacy range 只保存第一段，供 V2 迁移期读取。
fn validate_manual_associations(
    entry_id: Uuid,
    sentence_id: Uuid,
    variants: &[(Dialect, String)],
    inputs: &[ManualAssociationInput],
    existing: &HashMap<Uuid, SentenceAssociationRecord>,
    targets: &HashMap<(Uuid, Option<Uuid>), PublishedAssociationTarget>,
) -> (Vec<NewSentenceAssociation>, Vec<DraftValidationIssue>) {
    let mut issues = Vec::new();
    let mut rows: Vec<NewSentenceAssociation> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for input in inputs {
        let issue =
            |code: &str, message: &str| reference_issue(sentence_id, "associations", code, message);
        if !seen_ids.insert(input.id) {
            issues.push(issue(
                "sentence_association_duplicate_id",
                "同一条例句里的关联 ID 不能重复",
            ));
            continue;
        }
        let dialect = dialect_name(input.source_dialect);
        let Some((_, text)) = variants
            .iter()
            .find(|(candidate, _)| *candidate == input.source_dialect)
        else {
            issues.push(issue(
                "sentence_association_dialect_unavailable",
                "关联指向的方言侧在这条例句里不存在",
            ));
            continue;
        };
        if input.source_segments.is_empty() || input.source_segments.len() > 20 {
            issues.push(issue(
                "sentence_association_segments_invalid",
                "关联必须包含 1 到 20 个有序片段",
            ));
            continue;
        }

        let mut source_segments = Vec::with_capacity(input.source_segments.len());
        let mut previous_end = None;
        let mut segment_error = None;
        for segment in &input.source_segments {
            let Some(slice) = codepoint_slice(text, segment.start, segment.end) else {
                segment_error = Some((
                    "sentence_association_range_invalid",
                    "关联片段必须是正文内非空的 [start, end)",
                ));
                break;
            };
            if slice != segment.surface {
                segment_error = Some((
                    "sentence_association_surface_mismatch",
                    "关联片段与 surface 对不上，正文可能已经变了",
                ));
                break;
            }
            let (Ok(range_start), Ok(range_end), true) = (
                i32::try_from(segment.start),
                i32::try_from(segment.end),
                is_storable_surface(&segment.surface),
            ) else {
                segment_error = Some((
                    "sentence_association_range_invalid",
                    "关联片段必须落在正文内，且首尾不含空白、不超过 200 个码点",
                ));
                break;
            };
            if previous_end.is_some_and(|end| end > range_start) {
                segment_error = Some((
                    "sentence_association_segments_invalid",
                    "关联片段必须按正文顺序排列且互不重叠",
                ));
                break;
            }
            previous_end = Some(range_end);
            source_segments.push(NewSentenceAssociationSegment {
                range_start,
                range_end,
                surface: segment.surface.clone(),
            });
        }
        if let Some((code, message)) = segment_error {
            issues.push(issue(code, message));
            continue;
        }
        if rows.iter().any(|row| {
            row.source_dialect == dialect
                && segments_overlap(&row.source_segments, &source_segments)
        }) {
            issues.push(issue(
                "sentence_association_range_overlap",
                "同一侧正文里的关联片段不能重叠",
            ));
            continue;
        }

        let canonical_surface = input
            .source_segments
            .iter()
            .map(|segment| segment.surface.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if canonical_surface.chars().count() > 200 {
            issues.push(issue(
                "sentence_association_segments_invalid",
                "关联片段拼接后的词面不能超过 200 个码点",
            ));
            continue;
        }
        let first = source_segments.first().expect("segments were checked");
        let range_start = first.range_start;
        let range_end = first.range_end;
        let legacy_surface = first.surface.clone();
        let fingerprint = (input.association_schema_version == 3)
            .then(|| segments_fingerprint(sentence_id, dialect, text, &source_segments));

        let linked_shape = match (
            input.target_word_id,
            input.target_sense_id,
            input.target_publication_id,
            input.target_form_variant_id,
            input.pending_target_kind,
            input.pending_target_headword.as_deref(),
            input.pending_target_gloss.as_deref(),
        ) {
            (
                Some(target_word_id),
                Some(target_sense_id),
                target_publication_id,
                target_form_variant_id,
                None,
                None,
                None,
            ) if (input.association_schema_version == 2
                && target_publication_id.is_none()
                && target_form_variant_id.is_none())
                || (input.association_schema_version == 3
                    && target_publication_id.is_some() == target_form_variant_id.is_some()) =>
            {
                Some((target_word_id, target_sense_id))
            }
            (None, None, None, None, Some(pending_kind), Some(pending_headword), pending_gloss) => {
                let pending_headword = pending_headword.trim();
                let pending_gloss = pending_gloss
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let token_count = tokenize(&canonical_surface).len();
                let pending_kind = match pending_kind {
                    EntryKind::Word if token_count == 1 => "word",
                    EntryKind::Phrase
                        if token_count >= 2
                            && (input.association_schema_version == 3
                                || is_contiguous_phrase_surface(&canonical_surface)) =>
                    {
                        "phrase"
                    }
                    EntryKind::Phrase => {
                        issues.push(issue(
                            "sentence_association_pending_phrase_invalid",
                            "Pending 短语必须包含至少两个按原句顺序选择的单词",
                        ));
                        continue;
                    }
                    EntryKind::Word => {
                        issues.push(issue(
                            "sentence_association_pending_word_invalid",
                            "Pending 单词只能包含一个单词",
                        ));
                        continue;
                    }
                };
                let normalized_source =
                    crate::lexicon::normalization::normalize_headword(&canonical_surface);
                let normalized_pending =
                    crate::lexicon::normalization::normalize_headword(pending_headword);
                let (Ok(normalized_source), Ok(normalized_pending)) =
                    (normalized_source, normalized_pending)
                else {
                    issues.push(issue(
                        "sentence_association_pending_target_invalid",
                        "Pending 目标词面必须是可规范化的英文词或短语",
                    ));
                    continue;
                };
                if normalized_source.key != normalized_pending.key {
                    issues.push(issue(
                        "sentence_association_pending_surface_mismatch",
                        "Pending 目标词面必须与所选片段拼接后的词面一致",
                    ));
                    continue;
                }
                if pending_headword.chars().count() > 200
                    || pending_gloss.is_some_and(|value| value.chars().count() > 5000)
                {
                    issues.push(issue(
                        "sentence_association_pending_target_invalid",
                        "Pending 目标词面或预填词义超过长度限制",
                    ));
                    continue;
                }
                rows.push(NewSentenceAssociation {
                    id: input.id,
                    sentence_id,
                    source_dialect: dialect.to_owned(),
                    association_schema_version: input.association_schema_version,
                    source_segments,
                    segments_fingerprint: fingerprint,
                    range_start,
                    range_end,
                    surface: legacy_surface,
                    state: "pending".to_owned(),
                    target_entry_id: None,
                    target_sense_id: None,
                    target_form_slot_id: None,
                    target_publication_id: None,
                    target_form_variant_id: None,
                    target_component_usages_snapshot: None,
                    origin: "manual".to_owned(),
                    target_headword_snapshot: None,
                    target_gloss_snapshot: None,
                    resolved_pos: None,
                    resolved_form_type: None,
                    pending_target_kind: Some(pending_kind.to_owned()),
                    pending_target_headword: Some(pending_headword.to_owned()),
                    normalized_pending_target_headword: Some(normalized_pending.key),
                    pending_target_gloss: pending_gloss.map(str::to_owned),
                });
                continue;
            }
            _ => None,
        };
        let Some((target_word_id, target_sense_id)) = linked_shape else {
            issues.push(issue(
                "sentence_association_target_shape_invalid",
                "关联目标必须是完整的已发布词义或待关联词条形状",
            ));
            continue;
        };
        if target_word_id == entry_id {
            issues.push(issue(
                "sentence_association_self_target",
                "例句关联不能指向当前词条自身",
            ));
            continue;
        }
        let Some(target) = targets.get(&(target_word_id, input.target_publication_id)) else {
            issues.push(issue(
                "sentence_association_target_unavailable",
                "关联目标必须是未归档词条当前发布版本中的有效词义",
            ));
            continue;
        };
        let normalized = crate::lexicon::normalization::normalize_headword(&canonical_surface)
            .map(|normalized| normalized.key)
            .unwrap_or_default();
        let Some(resolved) = manual_target(
            target,
            target_sense_id,
            &normalized,
            input.target_publication_id,
            input.target_form_variant_id,
        ) else {
            issues.push(issue(
                "sentence_association_target_unavailable",
                "关联目标必须是未归档词条当前发布版本中的有效词义",
            ));
            continue;
        };
        let unchanged = input.association_schema_version == 2
            && existing.get(&input.id).is_some_and(|record| {
                record.source_dialect == dialect
                    && record.range_start == range_start
                    && record.range_end == range_end
                    && record.surface == legacy_surface
                    && record.state == "linked"
                    && record.target_entry_id == Some(target_word_id)
                    && record.target_sense_id == Some(target_sense_id)
            });
        rows.push(NewSentenceAssociation {
            id: input.id,
            sentence_id,
            source_dialect: dialect.to_owned(),
            association_schema_version: input.association_schema_version,
            source_segments,
            segments_fingerprint: fingerprint,
            range_start,
            range_end,
            surface: legacy_surface,
            state: "linked".to_owned(),
            target_entry_id: Some(target_word_id),
            target_sense_id: Some(resolved.target_sense_id),
            target_form_slot_id: resolved.target_form_slot_id,
            target_publication_id: resolved.target_publication_id,
            target_form_variant_id: resolved.target_form_variant_id,
            target_component_usages_snapshot: resolved
                .target_form_variant_id
                .and_then(|_| serde_json::to_value(&resolved.target_component_usages).ok()),
            origin: if unchanged {
                existing
                    .get(&input.id)
                    .map(|record| record.origin.clone())
                    .unwrap_or_else(|| "manual".to_owned())
            } else {
                "manual".to_owned()
            },
            target_headword_snapshot: Some(resolved.target_headword),
            target_gloss_snapshot: Some(resolved.target_gloss),
            resolved_pos: Some(resolved.resolved_pos),
            resolved_form_type: resolved.resolved_form_type,
            pending_target_kind: None,
            pending_target_headword: None,
            normalized_pending_target_headword: None,
            pending_target_gloss: None,
        });
    }
    (rows, issues)
}

#[cfg(test)]
#[path = "sentence_association_tests.rs"]
mod tests;
