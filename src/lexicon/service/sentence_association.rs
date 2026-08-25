use super::*;

use crate::lexicon::{
    model::{
        NewSentenceAssociation, PublishedFormSurfaceRecord, SentenceAssociationRecord,
        SentenceAssociationScanRecord,
    },
    node_identity::{dialect_from_name, dialect_name},
    sentence_association::{
        RESOLVER_VERSION, SentenceToken, associable_pos, codepoint_slice, is_stopword,
        is_storable_surface, text_hash, tokenize,
    },
};

const REPLACE_ASSOCIATIONS_SCOPE: &str = "lexicon.sentence.associations.replace";

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
    Some(WordSentenceAssociationV2 {
        id: record.id,
        source_dialect,
        source_range: SentenceSourceRangeV1 {
            start,
            end,
            surface: record.surface,
        },
        target_word_id: record.target_entry_id,
        target_sense_id: record.target_sense_id,
        target_form_slot_id: record.target_form_slot_id,
        origin: match record.origin.as_str() {
            "manual" => SentenceAssociationOriginV2::Manual,
            _ => SentenceAssociationOriginV2::Auto,
        },
        target_headword: record.target_headword_snapshot,
        target_gloss: record.target_gloss_snapshot,
        resolved_pos: record.resolved_pos,
        resolved_form_type: record.resolved_form_type,
    })
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
    variant_ids: Vec<Uuid>,
    normalized_surfaces: Vec<String>,
}

#[derive(Debug)]
struct PublishedAssociationSense {
    id: Uuid,
    gloss: String,
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
struct PublishedAssociationTarget {
    schema_version: i16,
    id: Uuid,
    headword: String,
    pos: Vec<PublishedAssociationPos>,
}

impl PublishedAssociationTarget {
    fn from_v2(word: AdminWordV2) -> Self {
        let pos =
            word.forms
                .pos
                .iter()
                .map(|forms| {
                    let mut slots = Vec::new();
                    slots.push(PublishedAssociationForm {
                        id: forms.base_form.id,
                        form_type: forms.base_form.form_type.clone(),
                        variant_ids: forms
                            .base_form
                            .variants
                            .iter()
                            .map(|variant| variant.id)
                            .collect(),
                        normalized_surfaces: normalized_v2_surfaces(&forms.base_form.variants),
                    });
                    slots.extend(forms.form_groups.iter().flat_map(|group| &group.slots).map(
                        |slot| PublishedAssociationForm {
                            id: slot.id,
                            form_type: slot.form_type.clone(),
                            variant_ids: slot.variants.iter().map(|variant| variant.id).collect(),
                            normalized_surfaces: normalized_v2_surfaces(&slot.variants),
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
        Self {
            schema_version: 2,
            id: word.id,
            headword: published_word_headword(&word),
            pos,
        }
    }

    fn from_v3(word: AdminWordV3) -> Result<Self, LexiconServiceError> {
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
                        PublishedAssociationForm {
                            id: form.id,
                            form_type: v3_form_type_name(form.form_type).to_owned(),
                            variant_ids: variants.iter().map(|(id, _)| *id).collect(),
                            normalized_surfaces: variants
                                .iter()
                                .filter_map(|(_, spelling)| normalized_surface(spelling))
                                .collect(),
                        }
                    })
                    .collect(),
                senses: association_senses(&meanings, forms.pos_id),
            })
            .collect();
        Ok(Self {
            schema_version: 3,
            id: word.id,
            headword: word.presentation.label,
            pos,
        })
    }

    fn from_snapshot(
        snapshot: serde_json::Value,
        allow_v3: bool,
    ) -> Result<Self, LexiconServiceError> {
        let version = snapshot
            .get("schema_version")
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i16::try_from(value).ok())
            .unwrap_or(-1);
        match version {
            2 => Ok(Self::from_v2(v2_publication_snapshot(snapshot)?)),
            3 if allow_v3 => {
                Self::from_v3(serde_json::from_value(snapshot).map_err(serialization_error)?)
            }
            3 => Err(LexiconServiceError::V3StorageUnavailable),
            version => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
        }
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
                form.variant_ids
                    .iter()
                    .any(|variant_id| variant_ids.contains(variant_id))
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

    fn manual_target(&self, sense_id: Uuid, normalized_surface: &str) -> Option<ResolvedTarget> {
        let pos = self
            .pos
            .iter()
            .find(|pos| pos.senses.iter().any(|sense| sense.id == sense_id))?;
        let sense = pos.senses.iter().find(|sense| sense.id == sense_id)?;
        let matching_slots = pos
            .forms
            .iter()
            .filter(|form| {
                form.normalized_surfaces
                    .iter()
                    .any(|surface| surface == normalized_surface)
            })
            .map(|form| (form.id, form.form_type.clone()))
            .collect::<Vec<_>>();
        // V2 沿用历史的「按目录顺序取首个槽位」行为；V3 允许同类 concrete form
        // 重复，不能把第一个误当成管理员选择，只有唯一命中时才固化 form_id。
        let slot = if self.schema_version == 2 {
            matching_slots.into_iter().next()
        } else {
            (matching_slots.len() == 1)
                .then(|| matching_slots.into_iter().next())
                .flatten()
        };
        Some(ResolvedTarget {
            target_entry_id: self.id,
            target_sense_id: sense_id,
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
                    gloss: published_sense_gloss(sense),
                })
                .collect()
        })
        .unwrap_or_default()
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

fn v3_form_variants(variants: &crate::lexicon::dto::WordRegionalVariantsV3) -> Vec<(Uuid, &str)> {
    use crate::lexicon::dto::WordRegionalVariantsV3;

    match variants {
        WordRegionalVariantsV3::Common { common } => vec![(common.id, common.spelling.as_str())],
        WordRegionalVariantsV3::UkUs { uk, us } => {
            vec![(uk.id, uk.spelling.as_str()), (us.id, us.spelling.as_str())]
        }
    }
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
) -> Option<ResolvedTarget> {
    target.manual_target(sense_id, normalized_surface)
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
        let mut legacy_meanings: DraftMeaningsStepContent =
            serde_json::from_value(serde_json::to_value(&*meanings).map_err(serialization_error)?)
                .map_err(serialization_error)?;
        let associations = LexiconRepository::sentence_associations(&mut **tx, entry_id)
            .await
            .map_err(repository_error)?;
        let scans = LexiconRepository::sentence_association_scans(&mut **tx, entry_id)
            .await
            .map_err(repository_error)?;
        apply_sentence_associations(&mut legacy_meanings, associations, scans);
        *meanings = serde_json::from_value(
            serde_json::to_value(legacy_meanings).map_err(serialization_error)?,
        )
        .map_err(serialization_error)?;
        Ok(())
    }

    pub(super) async fn hydrate_v3_sentence_associations(
        &self,
        entry_id: Uuid,
        meanings: &mut crate::lexicon::dto::DraftMeaningsStepContentV3,
    ) -> Result<(), LexiconServiceError> {
        let mut legacy_meanings: DraftMeaningsStepContent =
            serde_json::from_value(serde_json::to_value(&*meanings).map_err(serialization_error)?)
                .map_err(serialization_error)?;
        let associations =
            LexiconRepository::sentence_associations(self.repository.pool(), entry_id)
                .await
                .map_err(repository_error)?;
        let scans = LexiconRepository::sentence_association_scans(self.repository.pool(), entry_id)
            .await
            .map_err(repository_error)?;
        apply_sentence_associations(&mut legacy_meanings, associations, scans);
        *meanings = serde_json::from_value(
            serde_json::to_value(legacy_meanings).map_err(serialization_error)?,
        )
        .map_err(serialization_error)?;
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
    ) -> Result<(), LexiconServiceError> {
        let variants = sentence_variants(meanings);
        let live_sentence_ids = variants
            .iter()
            .map(|variant| variant.sentence_id)
            .collect::<Vec<_>>();
        let live_dialects = variants
            .iter()
            .map(|variant| dialect_name(variant.dialect).to_owned())
            .collect::<Vec<_>>();
        LexiconRepository::prune_sentence_associations(
            tx,
            entry_id,
            &live_sentence_ids,
            &live_dialects,
        )
        .await
        .map_err(repository_error)?;

        let scanned = LexiconRepository::sentence_association_scans(&mut **tx, entry_id)
            .await
            .map_err(repository_error)?
            .into_iter()
            .filter(|scan| scan.resolver_version == RESOLVER_VERSION)
            .map(|scan| ((scan.sentence_id, scan.source_dialect), scan.text_hash))
            .collect::<HashMap<_, _>>();

        let pending = variants
            .into_iter()
            .filter_map(|variant| {
                let hash = text_hash(variant.text);
                let key = (
                    variant.sentence_id,
                    dialect_name(variant.dialect).to_owned(),
                );
                (scanned.get(&key) != Some(&hash)).then_some((variant, hash))
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }

        let tokenized = pending
            .iter()
            .map(|(variant, _)| {
                tokenize(variant.text)
                    .into_iter()
                    .filter(|token| !is_stopword(&token.normalized))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let mut scopes = pending
            .iter()
            .flat_map(|(variant, _)| lookup_scopes(variant.dialect))
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

        for ((variant, hash), tokens) in pending.into_iter().zip(tokenized) {
            let dialect = dialect_name(variant.dialect);
            let scopes = lookup_scopes(variant.dialect);
            let associations = tokens
                .iter()
                .filter_map(|token| {
                    let target = resolve_token(token, scopes, &candidates, &snapshots)?;
                    Some(NewSentenceAssociation {
                        id: Uuid::now_v7(),
                        sentence_id: variant.sentence_id,
                        source_dialect: dialect.to_owned(),
                        range_start: i32::try_from(token.start).ok()?,
                        range_end: i32::try_from(token.end).ok()?,
                        surface: token.surface.clone(),
                        target_entry_id: target.target_entry_id,
                        target_sense_id: target.target_sense_id,
                        target_form_slot_id: target.target_form_slot_id,
                        origin: "auto".to_owned(),
                        target_headword_snapshot: target.target_headword,
                        target_gloss_snapshot: target.target_gloss,
                        resolved_pos: target.resolved_pos,
                        resolved_form_type: target.resolved_form_type,
                    })
                })
                .collect::<Vec<_>>();
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
    /// 只对「当前正文已经解析过」的例句开放（`associations_state = resolved`）。
    /// 草稿从未发布、或正文改了还没重新发布时，库里的区间对不上当前正文，
    /// 此时允许编辑只会写进一批下次发布就被冲掉的数据。
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
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        if input.base_revision < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_revision",
                message: "base_revision must be at least 1",
            });
        }
        if input.base_lifecycle_revision < 1 {
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
        ensure_association_source_state(&word, input.base_revision, input.base_lifecycle_revision)?;

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
        let scanned = LexiconRepository::sentence_association_scans(&mut *transaction, entry_id)
            .await
            .map_err(repository_error)?
            .into_iter()
            .filter(|scan| scan.sentence_id == sentence_id)
            .filter(|scan| scan.resolver_version == RESOLVER_VERSION)
            .map(|scan| (scan.source_dialect, scan.text_hash))
            .collect::<HashMap<_, _>>();
        let resolved = !variants.is_empty()
            && variants.iter().all(|(dialect, text)| {
                scanned.get(dialect_name(*dialect)) == Some(&text_hash(text))
            });
        if !resolved {
            return Err(LexiconServiceError::SentenceAssociationsUnresolved);
        }

        let existing = LexiconRepository::sentence_associations(&mut *transaction, entry_id)
            .await
            .map_err(repository_error)?
            .into_iter()
            .filter(|association| association.sentence_id == sentence_id)
            .map(|association| (association.id, association))
            .collect::<HashMap<_, _>>();

        let mut target_ids = input
            .associations
            .iter()
            .map(|association| association.target_word_id)
            .filter(|target_id| *target_id != entry_id)
            .collect::<Vec<_>>();
        target_ids.sort_unstable();
        target_ids.dedup();
        let targets =
            LexiconRepository::current_publication_snapshots(&mut transaction, &target_ids)
                .await
                .map_err(repository_error)?
                .into_iter()
                .map(|record| {
                    PublishedAssociationTarget::from_snapshot(record.snapshot, allow_v3)
                        .map(|target| (record.entry_id, target))
                })
                .collect::<Result<HashMap<_, _>, _>>()?;

        let (rows, issues) = validate_manual_associations(
            entry_id,
            sentence_id,
            &variants,
            &input.associations,
            &existing,
            &targets,
        );
        if !issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(issues));
        }

        for (dialect, _) in &variants {
            let dialect = dialect_name(*dialect);
            let side = rows
                .iter()
                .filter(|row| row.source_dialect == dialect)
                .cloned()
                .collect::<Vec<_>>();
            LexiconRepository::replace_sentence_associations(
                &mut transaction,
                entry_id,
                sentence_id,
                dialect,
                &side,
                None,
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
        AdminWordAny::V3(word) => serde_json::from_value(
            serde_json::to_value(&word.meanings).map_err(serialization_error)?,
        )
        .map_err(serialization_error),
    }
}

/// 逐条校验人工关联，顺带把只读投影解析出来。返回待写入的行与全部 issue；
/// 有 issue 时行不会被使用，但仍然全量校验，一次把所有问题报给管理员。
fn validate_manual_associations(
    entry_id: Uuid,
    sentence_id: Uuid,
    variants: &[(Dialect, String)],
    inputs: &[SentenceAssociationInputV2],
    existing: &HashMap<Uuid, SentenceAssociationRecord>,
    targets: &HashMap<Uuid, PublishedAssociationTarget>,
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
        let Some(slice) = codepoint_slice(text, input.source_range.start, input.source_range.end)
        else {
            issues.push(issue(
                "sentence_association_range_invalid",
                "关联区间必须是正文内非空的 [start, end)",
            ));
            continue;
        };
        if slice != input.source_range.surface {
            issues.push(issue(
                "sentence_association_surface_mismatch",
                "关联区间与 surface 对不上，正文可能已经变了",
            ));
            continue;
        }
        // 库层对 surface 还有 btrim 与 200 码点两条约束。不在这里挡住的话，
        // 框选时多带一个尾随空格就会一路走到 INSERT，以 CHECK 违例变成 500。
        let (Ok(range_start), Ok(range_end), true) = (
            i32::try_from(input.source_range.start),
            i32::try_from(input.source_range.end),
            is_storable_surface(&input.source_range.surface),
        ) else {
            issues.push(issue(
                "sentence_association_range_invalid",
                "关联区间必须落在正文内，且首尾不含空白、不超过 200 个码点",
            ));
            continue;
        };
        if rows.iter().any(|row| {
            row.source_dialect == dialect
                && row.range_start < range_end
                && range_start < row.range_end
        }) {
            issues.push(issue(
                "sentence_association_range_overlap",
                "同一侧正文里的关联区间不能重叠",
            ));
            continue;
        }
        if input.target_word_id == entry_id {
            issues.push(issue(
                "sentence_association_self_target",
                "例句关联不能指向当前词条自身",
            ));
            continue;
        }
        let Some(target) = targets.get(&input.target_word_id) else {
            issues.push(issue(
                "sentence_association_target_unavailable",
                "关联目标必须是未归档词条当前发布版本中的有效词义",
            ));
            continue;
        };
        let normalized =
            crate::lexicon::normalization::normalize_headword(&input.source_range.surface)
                .map(|normalized| normalized.key)
                .unwrap_or_default();
        let Some(resolved) = manual_target(target, input.target_sense_id, &normalized) else {
            issues.push(issue(
                "sentence_association_target_unavailable",
                "关联目标必须是未归档词条当前发布版本中的有效词义",
            ));
            continue;
        };
        // 与库里逐字相同的那条保持原样：整组替换不该把没动过的自动关联改写成人工。
        let unchanged = existing.get(&input.id).is_some_and(|record| {
            record.source_dialect == dialect
                && record.range_start == range_start
                && record.range_end == range_end
                && record.surface == input.source_range.surface
                && record.target_entry_id == input.target_word_id
                && record.target_sense_id == input.target_sense_id
        });
        rows.push(NewSentenceAssociation {
            id: input.id,
            sentence_id,
            source_dialect: dialect.to_owned(),
            range_start,
            range_end,
            surface: input.source_range.surface.clone(),
            target_entry_id: input.target_word_id,
            target_sense_id: resolved.target_sense_id,
            target_form_slot_id: resolved.target_form_slot_id,
            origin: if unchanged {
                existing
                    .get(&input.id)
                    .map(|record| record.origin.clone())
                    .unwrap_or_else(|| "manual".to_owned())
            } else {
                "manual".to_owned()
            },
            target_headword_snapshot: resolved.target_headword,
            target_gloss_snapshot: resolved.target_gloss,
            resolved_pos: resolved.resolved_pos,
            resolved_form_type: resolved.resolved_form_type,
        });
    }
    (rows, issues)
}

#[cfg(test)]
#[path = "sentence_association_tests.rs"]
mod tests;
