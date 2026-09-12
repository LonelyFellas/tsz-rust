use super::*;

use super::v3::ComponentTargetWord;
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
        RESOLVER_VERSION, SentenceToken, associable_pos, is_stopword, text_hash, tokenize,
    },
};

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
    super::text_links::apply_manual(meanings);
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
}

#[derive(Debug)]
struct PublishedAssociationVariant {
    id: Uuid,
    dialect: Dialect,
    spelling: String,
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
    id: Uuid,
    kind: EntryKind,
    headword: String,
    pos: Vec<PublishedAssociationPos>,
}

impl PublishedAssociationTarget {
    fn from_v3(word: AdminWordV3) -> Result<Self, LexiconServiceError> {
        Self::from_v3_parts(
            word.id,
            word.kind,
            word.presentation.label,
            &word.forms,
            &word.meanings,
        )
    }

    /// 从未发布的草稿目标：内容来自草稿投影，没有发布版本。
    pub(super) fn from_component_target(
        word: ComponentTargetWord,
    ) -> Result<Self, LexiconServiceError> {
        Self::from_v3_parts(word.id, word.kind, word.label, &word.forms, &word.meanings)
    }

    fn from_v3_parts(
        id: Uuid,
        kind: WordEntryKindV3,
        label: String,
        forms_content: &crate::lexicon::dto::DraftFormsStepContentV3,
        meanings_content: &DraftMeaningsStepContentV3,
    ) -> Result<Self, LexiconServiceError> {
        let meanings =
            crate::lexicon::sentence_association::v3_meanings_to_relational(meanings_content)
                .map_err(serialization_error)?;
        let pos = forms_content
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
                                    candidate.id == member.form_id && candidate.form_type == "base"
                                })
                            })
                            .map(|base| base.id)
                            .collect::<Vec<_>>();
                        if base_form_ids.is_empty() && form.form_type == "base" {
                            base_form_ids.push(form.id);
                        }
                        base_form_ids.sort_unstable();
                        base_form_ids.dedup();
                        PublishedAssociationForm {
                            id: form.id,
                            form_type: v3_form_type_name(&form.form_type).to_owned(),
                            base_form_ids,
                            variants: variants
                                .iter()
                                .map(|(id, dialect, spelling, component_usages)| {
                                    PublishedAssociationVariant {
                                        id: *id,
                                        dialect: *dialect,
                                        spelling: (*spelling).to_owned(),
                                        component_usages: component_usages.to_vec(),
                                    }
                                })
                                .collect(),
                        }
                    })
                    .collect(),
                senses: association_senses_v3(meanings_content, &meanings, forms.pos_id),
            })
            .collect();
        Ok(Self {
            id,
            kind: match kind {
                WordEntryKindV3::Word => EntryKind::Word,
                WordEntryKindV3::Phrase => EntryKind::Phrase,
            },
            headword: label,
            pos,
        })
    }

    pub(super) fn from_snapshot(
        snapshot: serde_json::Value,
        allow_v3: bool,
    ) -> Result<Self, LexiconServiceError> {
        let version = snapshot
            .get("schema_version")
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i16::try_from(value).ok())
            .unwrap_or(-1);
        match version {
            3 if allow_v3 => {
                Self::from_v3(serde_json::from_value(snapshot).map_err(serialization_error)?)
            }
            3 => Err(LexiconServiceError::V3StorageUnavailable),
            version => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
        }
    }

    pub(super) fn sentence_discovery_candidates(
        &self,
        // 草稿目标没有发布版本，候选与词义的 `publication_id` 都留空。
        publication_id: Option<Uuid>,
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
                            form_type: form_type.clone(),
                            spelling: variant.spelling.clone(),
                            dialect: variant.dialect,
                            base_form_ids: if true {
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
                matched_form_type: form_type.clone(),
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
    crate::lexicon::form_types::parse_code(value).ok()
}

fn v3_form_type_name(form_type: &str) -> &str {
    form_type
}

impl LexiconService {
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
                PublishedAssociationTarget::from_snapshot(record.snapshot, allow_v3_targets)
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

#[cfg(test)]
#[path = "sentence_association_tests.rs"]
mod tests;
