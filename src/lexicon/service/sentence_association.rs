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

/// 词性下只有一个词义才算解析成功——多义即歧义，一律跳过。
fn resolve_unique_sense(word: &AdminWordV2, pos_id: Uuid) -> Option<&WordSenseV2> {
    let pos = word.meanings.pos.iter().find(|pos| pos.pos_id == pos_id)?;
    match pos.senses.as_slice() {
        [sense] => Some(sense),
        _ => None,
    }
}

/// 命中的词形变体属于哪个词形槽位。槽位才是「按读者方言换拼写」的锚点，
/// 变体本身是某一侧的拼写。
fn locate_form_slot(word: &AdminWordV2, pos_id: Uuid, variant_id: Uuid) -> Option<(Uuid, String)> {
    let pos = word.forms.pos.iter().find(|pos| pos.pos_id == pos_id)?;
    if pos
        .base_form
        .variants
        .iter()
        .any(|variant| variant.id == variant_id)
    {
        return Some((pos.base_form.id, pos.base_form.form_type.clone()));
    }
    pos.form_groups
        .iter()
        .flat_map(|group| &group.slots)
        .find(|slot| slot.variants.iter().any(|variant| variant.id == variant_id))
        .map(|slot| (slot.id, slot.form_type.clone()))
}

/// 人工补关联时按词面找槽位：管理员选的是文字和词义，没有变体 ID 可用。
/// 词库里没有这个词形是允许的（管理员可能比词库知道得多），此时槽位缺省。
fn locate_form_slot_by_surface(
    word: &AdminWordV2,
    pos_id: Uuid,
    normalized_surface: &str,
) -> Option<(Uuid, String)> {
    let pos = word.forms.pos.iter().find(|pos| pos.pos_id == pos_id)?;
    let matches = |variants: &[WordFormVariantV2]| {
        variants.iter().any(|variant| {
            crate::lexicon::normalization::normalize_headword(&variant.spelling)
                .is_ok_and(|normalized| normalized.key == normalized_surface)
        })
    };
    if matches(&pos.base_form.variants) {
        return Some((pos.base_form.id, pos.base_form.form_type.clone()));
    }
    pos.form_groups
        .iter()
        .flat_map(|group| &group.slots)
        .find(|slot| matches(&slot.variants))
        .map(|slot| (slot.id, slot.form_type.clone()))
}

/// 某个词义所属的词性节点与词性代码。
fn sense_pos(word: &AdminWordV2, sense_id: Uuid) -> Option<(Uuid, String)> {
    let pos = word
        .meanings
        .pos
        .iter()
        .find(|pos| pos.senses.iter().any(|sense| sense.id == sense_id))?;
    let code = word
        .forms
        .pos
        .iter()
        .find(|forms| forms.pos_id == pos.pos_id)
        .map(|forms| forms.pos.clone())?;
    Some((pos.pos_id, code))
}

pub(super) fn manual_target(
    word: &AdminWordV2,
    sense_id: Uuid,
    normalized_surface: &str,
) -> Option<ResolvedTarget> {
    let (pos_id, pos_code) = sense_pos(word, sense_id)?;
    let sense = word
        .meanings
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .find(|sense| sense.id == sense_id)?;
    let slot = locate_form_slot_by_surface(word, pos_id, normalized_surface);
    Some(ResolvedTarget {
        target_entry_id: word.id,
        target_sense_id: sense_id,
        target_form_slot_id: slot.as_ref().map(|(id, _)| *id),
        target_headword: published_word_headword(word),
        target_gloss: published_sense_gloss(sense),
        resolved_pos: pos_code,
        resolved_form_type: slot.map(|(_, form_type)| form_type),
    })
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

        let candidates =
            LexiconRepository::published_form_surfaces(tx, entry_id, &scopes, &surfaces)
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
                serde_json::from_value::<AdminWordV2>(record.snapshot)
                    .map(|word| (record.entry_id, word))
                    .map_err(serialization_error)
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
    snapshots: &HashMap<Uuid, AdminWordV2>,
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

    let word = snapshots.get(&entry_id)?;
    let sense = resolve_unique_sense(word, pos_id)?;
    // 同一个词性下可能有多个槽位共用一个拼写——不规则动词 cut 的原形、过去式、
    // 过去分词都是 cut。词义仍然唯一，所以关联成立；但槽位是哪一个没有证据，
    // 按变体 ID 的大小随手挑一个等于给猜测背书，这里留空，前端回落到原句词面。
    // 按槽位 ID 去重要用有序集合：matched 是按变体 ID 排的，同一槽位的两个方言变体
    // 之间可能夹着别的槽位，Vec::dedup_by 只并相邻项会漏掉。
    let slots = matched
        .iter()
        .filter_map(|(_, _, variant_id)| locate_form_slot(word, pos_id, *variant_id))
        .collect::<BTreeMap<Uuid, String>>();
    let slot = (slots.len() == 1)
        .then(|| slots.into_iter().next())
        .flatten();
    let pos_code = word
        .forms
        .pos
        .iter()
        .find(|pos| pos.pos_id == pos_id)
        .map(|pos| pos.pos.clone())?;
    Some(ResolvedTarget {
        target_entry_id: entry_id,
        target_sense_id: sense.id,
        target_form_slot_id: slot.as_ref().map(|(id, _)| *id),
        target_headword: published_word_headword(word),
        target_gloss: published_sense_gloss(sense),
        resolved_pos: pos_code,
        resolved_form_type: slot.map(|(_, form_type)| form_type),
    })
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
    pub async fn replace_sentence_associations(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        sentence_id: Uuid,
        idempotency_key: Uuid,
        input: ReplaceSentenceAssociationsInput,
    ) -> Result<AdminWordV2Envelope, LexiconServiceError> {
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
            transaction.commit().await.map_err(database_error)?;
            return serde_json::from_value(existing.response_body).map_err(serialization_error);
        }

        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        let mut word = entry_from_record(record)?;
        ensure_active(&word)?;
        ensure_revision(&word, input.base_revision)?;
        super::lifecycle::ensure_lifecycle_revision(&word, input.base_lifecycle_revision)?;

        let sentence = word
            .meanings
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
                    serde_json::from_value::<AdminWordV2>(record.snapshot)
                        .map(|target| (record.entry_id, target))
                        .map_err(serialization_error)
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

        word.lifecycle_revision += 1;
        word.updated_at = Utc::now();
        LexiconRepository::record_sentence_association_edit(
            &mut transaction,
            &word,
            actor_id,
            request_id,
            sentence_id,
        )
        .await
        .map_err(repository_error)?;
        Self::hydrate_sentence_associations_in(&mut transaction, &mut word).await?;
        LexiconRepository::insert_idempotent_word_response(
            &mut transaction,
            REPLACE_ASSOCIATIONS_SCOPE,
            actor_id,
            idempotency_key,
            &request_hash,
            Some(sentence_id),
            &word,
            200,
        )
        .await
        .map_err(repository_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(AdminWordV2Envelope { word })
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
    targets: &HashMap<Uuid, AdminWordV2>,
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
