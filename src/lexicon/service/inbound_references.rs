//! 本词条节点的入站引用：多维例句标注、其他词条当前发布版本里的词义引用、草稿关联词、短语成分用词。
//! 读接口与各写路径共用同一判定（引用在给定内容里是否仍成立），界面禁用与保存拦截口径一致。

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;
use sqlx::{Postgres, Row, Transaction};

use super::v3::{ComponentTargetScope, ComponentTargetWord, phrase_component_matches_target};
use super::*;
use crate::lexicon::{
    dto::{
        DialectVariantRichTextSlotV3, DraftMeaningsStepContentV3, EnglishTextV3,
        InboundReferenceKindV3, InboundReferenceNodeTypeV3, InboundReferenceNodeV3,
        InboundReferenceSourceV3, InboundReferenceTargetV3, InboundReferenceV3,
        InboundReferencesV3, TextLinkV3,
    },
    model::InboundSenseReferenceRecord,
    shared_sentences::SentenceTarget,
};

const MAX_INBOUND_REFERENCE_ITEMS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InboundReferenceCheck {
    /// 词形 / 词义保存：四类都查；被破坏的发布引用来源加共享锁（与原词形保存守卫一致）。
    DraftSave,
    /// 词形影响预览：范围同保存，只读不加锁。
    DraftPreview,
    /// 发布与切换版本：只换发布内容。草稿关联与成分用词指向草稿节点，不受影响。
    PublicationContent,
}

impl InboundReferenceCheck {
    const fn includes_draft_sources(self) -> bool {
        matches!(self, Self::DraftSave | Self::DraftPreview)
    }

    const fn locks(self) -> bool {
        !matches!(self, Self::DraftPreview)
    }
}

/// 引用是否成立的判定依据；目标节点 ID 在 `InboundReferenceV3.target` 里。
#[derive(Clone)]
enum Rule {
    SharedSentence {
        link: Option<Box<TextLinkV3>>,
        dialect: String,
    },
    /// 发布引用与草稿关联只指向词义：词义还在就成立。
    Sense,
    PhraseComponent {
        dialect: Option<Dialect>,
        form_type: String,
    },
}

#[derive(Clone)]
struct Candidate {
    reference: InboundReferenceV3,
    rule: Rule,
}

#[derive(sqlx::FromRow)]
struct PhraseComponentRow {
    id: Uuid,
    #[sqlx(rename = "entry_id")]
    source_entry_id: Uuid,
    source_sense_id: Option<Uuid>,
    #[sqlx(rename = "target_pos_id")]
    pos_id: Option<Uuid>,
    #[sqlx(rename = "target_base_form_id")]
    base_form_id: Option<Uuid>,
    #[sqlx(rename = "target_form_id")]
    form_id: Option<Uuid>,
    #[sqlx(rename = "target_variant_id")]
    variant_id: Option<Uuid>,
    #[sqlx(rename = "target_sense_id")]
    sense_id: Option<Uuid>,
    #[sqlx(rename = "target_dialect")]
    dialect: Option<String>,
    #[sqlx(rename = "target_form_type")]
    form_type: Option<String>,
}

fn parse_dialect(value: &str) -> Option<Dialect> {
    match value {
        "common" => Some(Dialect::Common),
        "uk" => Some(Dialect::Uk),
        "us" => Some(Dialect::Us),
        _ => None,
    }
}

fn reference(
    id: String,
    kind: InboundReferenceKindV3,
    target: InboundReferenceTargetV3,
    source: InboundReferenceSourceV3,
) -> InboundReferenceV3 {
    InboundReferenceV3 {
        id,
        kind,
        target,
        stale: false,
        source,
    }
}

/// 例句英文按标注方言取对应一侧；该侧缺失时退回另一侧。
fn sentence_text(text: &EnglishTextV3, dialect: &str) -> Option<String> {
    match text {
        EnglishTextV3::Unified { common } => Some(common.value.text().to_owned()),
        EnglishTextV3::Distinguish {
            source_dialect,
            uk,
            us,
        } => {
            let prefer_uk = match dialect {
                "uk" => true,
                "us" => false,
                _ => *source_dialect == SourceDialect::Uk,
            };
            let slots = if prefer_uk { [uk, us] } else { [us, uk] };
            slots.into_iter().find_map(|slot| match slot {
                DialectVariantRichTextSlotV3::Ready { variant } => {
                    Some(variant.value.text().to_owned())
                }
                DialectVariantRichTextSlotV3::Missing => None,
            })
        }
    }
}

async fn collect_candidates(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    retained_sense_ids: Option<&[Uuid]>,
    check: InboundReferenceCheck,
) -> Result<Vec<Candidate>, LexiconServiceError> {
    let mut candidates = Vec::new();

    // 1. 多维例句标注：口径同原 ensure_shared_sentence_targets（未删除例句、linked 目标，允许自指）。
    let rows = sqlx::query(
        r#"
        SELECT annotation.id, annotation.target_ref, annotation.source_segments,
               annotation.source_dialect, sentence.id AS sentence_id,
               sentence.revision AS sentence_revision,
               sentence.content -> 'en_text' AS en_text
        FROM lexicon.shared_sentence_annotations annotation
        JOIN lexicon.shared_sentences sentence ON sentence.id = annotation.sentence_id
        WHERE sentence.deleted_at IS NULL
          AND annotation.target_entry_id = $1
          AND annotation.target_ref IS NOT NULL
        ORDER BY sentence.id, annotation.id
        "#,
    )
    .bind(entry_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    for row in rows {
        let id: Uuid = row.get("id");
        let target: SentenceTarget =
            serde_json::from_value(row.get("target_ref")).map_err(serialization_error)?;
        let segments: Vec<SentenceSourceRangeV1> =
            serde_json::from_value(row.get("source_segments")).map_err(serialization_error)?;
        let dialect: String = row.get("source_dialect");
        let link = target.as_text_link(id, segments.clone()).map(Box::new);
        let target = link
            .as_ref()
            .map(|link| InboundReferenceTargetV3 {
                pos_id: Some(link.target_pos_id),
                base_form_id: Some(link.target_base_form_id),
                form_id: Some(link.target_form_id),
                variant_id: Some(link.target_variant_id),
                sense_id: Some(link.target_sense_id),
            })
            .unwrap_or_default();
        let text = row
            .get::<Option<Value>, _>("en_text")
            .and_then(|value| serde_json::from_value::<EnglishTextV3>(value).ok())
            .and_then(|text| sentence_text(&text, &dialect));
        candidates.push(Candidate {
            reference: reference(
                format!("shared_sentence:{id}"),
                InboundReferenceKindV3::SharedSentence,
                target,
                InboundReferenceSourceV3 {
                    sentence_id: Some(row.get("sentence_id")),
                    sentence_revision: Some(row.get("sentence_revision")),
                    sentence_text: text,
                    source_dialect: parse_dialect(&dialect),
                    segments: Some(segments),
                    ..InboundReferenceSourceV3::default()
                },
            ),
            rule: Rule::SharedSentence { link, dialect },
        });
    }

    // 2. 其他词条当前发布版本里的词义引用：口径同 current_inbound_sense_refs（排除已归档来源）。
    let publication_refs = match (retained_sense_ids, check.locks()) {
        (Some(retained), true) => {
            LexiconRepository::current_inbound_sense_refs(tx, entry_id, retained)
                .await
                .map_err(repository_error)?
        }
        _ => sqlx::query_as::<_, InboundSenseReferenceRecord>(
            r#"
            SELECT sense_ref.target_sense_id,
                   sense_ref.entry_id AS source_entry_id,
                   sense_ref.publication_id AS source_publication_id,
                   sense_ref.source_node_id,
                   sense_ref.reference_kind
            FROM lexicon.entry_publication_sense_refs sense_ref
            JOIN lexicon.entries source_entry
              ON source_entry.id = sense_ref.entry_id
             AND source_entry.current_publication_id = sense_ref.publication_id
             AND source_entry.archived_at IS NULL
            WHERE sense_ref.target_entry_id = $1
              AND ($2::uuid[] IS NULL OR NOT (sense_ref.target_sense_id = ANY($2::uuid[])))
            ORDER BY sense_ref.target_sense_id, sense_ref.entry_id, sense_ref.source_node_id
            "#,
        )
        .bind(entry_id)
        .bind(retained_sense_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)?,
    };
    for record in publication_refs {
        candidates.push(Candidate {
            reference: reference(
                format!(
                    "publication_sense_ref:{}:{}:{}",
                    record.source_publication_id, record.source_node_id, record.target_sense_id
                ),
                InboundReferenceKindV3::PublicationSenseRef,
                InboundReferenceTargetV3 {
                    sense_id: Some(record.target_sense_id),
                    ..InboundReferenceTargetV3::default()
                },
                InboundReferenceSourceV3 {
                    entry_id: Some(record.source_entry_id),
                    publication_id: Some(record.source_publication_id),
                    node_id: Some(record.source_node_id),
                    reference_kind: Some(record.reference_kind),
                    ..InboundReferenceSourceV3::default()
                },
            ),
            rule: Rule::Sense,
        });
    }

    if !check.includes_draft_sources() {
        return Ok(candidates);
    }

    // Saved bindings protect sense deletion even when the request omits the sense and its bindings.
    let bindings = sqlx::query_as::<_, (Uuid, Uuid, Uuid, i32)>(
        r#"
        WITH bindings AS (
            SELECT sense_id, form_group_id, entry_pos_id FROM lexicon.sense_form_group_bindings WHERE entry_id = $1
            UNION
            SELECT id, form_group_id, entry_pos_id FROM lexicon.senses WHERE entry_id = $1 AND form_group_id IS NOT NULL
        )
        SELECT binding.sense_id, binding.form_group_id, binding.entry_pos_id, form_group.ordinal
        FROM bindings binding JOIN lexicon.v3_form_groups form_group ON form_group.id = binding.form_group_id
        ORDER BY binding.sense_id, binding.form_group_id
        "#,
    )
    .bind(entry_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    for (sense_id, group_id, pos_id, ordinal) in bindings {
        candidates.push(Candidate {
            reference: reference(
                format!("form_group_sense_binding:{group_id}:{sense_id}"),
                InboundReferenceKindV3::FormGroupSenseBinding,
                InboundReferenceTargetV3 {
                    pos_id: Some(pos_id),
                    sense_id: Some(sense_id),
                    ..InboundReferenceTargetV3::default()
                },
                InboundReferenceSourceV3 {
                    entry_id: Some(entry_id),
                    node_id: Some(group_id),
                    form_group_label: Some(format!("第 {} 组词形变化", ordinal + 1)),
                    ..InboundReferenceSourceV3::default()
                },
            ),
            rule: Rule::Sense,
        });
    }

    // 3. 其他词条草稿里的关联词（排除自指与已归档来源）。
    let relations = sqlx::query_as::<_, (Uuid, Uuid, Uuid, String, Uuid)>(
        r#"
        SELECT relation.id, relation.entry_id, relation.source_sense_id,
               relation.relation_type, relation.target_sense_id
        FROM lexicon.relations relation
        JOIN lexicon.entries source_entry
          ON source_entry.id = relation.entry_id
         AND source_entry.archived_at IS NULL
        WHERE relation.target_entry_id = $1
          AND relation.target_sense_id IS NOT NULL
          AND relation.entry_id <> $1
          AND ($2::uuid[] IS NULL OR NOT (relation.target_sense_id = ANY($2::uuid[])))
        ORDER BY relation.target_sense_id, relation.entry_id, relation.id
        "#,
    )
    .bind(entry_id)
    .bind(retained_sense_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    for (id, source_entry_id, source_sense_id, relation_type, target_sense_id) in relations {
        candidates.push(Candidate {
            reference: reference(
                format!("draft_relation:{id}"),
                InboundReferenceKindV3::DraftRelation,
                InboundReferenceTargetV3 {
                    sense_id: Some(target_sense_id),
                    ..InboundReferenceTargetV3::default()
                },
                InboundReferenceSourceV3 {
                    entry_id: Some(source_entry_id),
                    sense_id: Some(source_sense_id),
                    node_id: Some(id),
                    relation_type: Some(relation_type),
                    ..InboundReferenceSourceV3::default()
                },
            ),
            rule: Rule::Sense,
        });
    }

    // 4. 短语草稿的成分用词：变体级 + 释义级 resolved 行，排除已归档来源。
    // 钉住了发布版本（target_publication_id 非空）的成分引用的是不可变快照，草稿改动伤不到它，
    // 不算入站引用；短语一旦发布，它会以类型 2 的发布引用出现。
    let components = sqlx::query_as::<_, PhraseComponentRow>(
        r#"
        SELECT usage.id, usage.entry_id, NULL::uuid AS source_sense_id,
               usage.target_pos_id, usage.target_base_form_id, usage.target_form_id,
               usage.target_variant_id, usage.target_sense_id,
               usage.target_dialect, usage.target_form_type
        FROM lexicon.v3_phrase_variant_component_usages usage
        JOIN lexicon.entries source_entry
          ON source_entry.id = usage.entry_id
         AND source_entry.archived_at IS NULL
        WHERE usage.target_entry_id = $1 AND usage.entry_id <> $1
          AND usage.state = 'resolved' AND usage.target_publication_id IS NULL
        UNION ALL
        SELECT usage.id, usage.entry_id, usage.sense_id AS source_sense_id,
               usage.target_pos_id, usage.target_base_form_id, usage.target_form_id,
               usage.target_variant_id, usage.target_sense_id,
               usage.target_dialect, usage.target_form_type
        FROM lexicon.v3_phrase_sense_component_usages usage
        JOIN lexicon.entries source_entry
          ON source_entry.id = usage.entry_id
         AND source_entry.archived_at IS NULL
        WHERE usage.target_entry_id = $1 AND usage.entry_id <> $1
          AND usage.state = 'resolved' AND usage.target_publication_id IS NULL
        ORDER BY 2, 1
        "#,
    )
    .bind(entry_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    for PhraseComponentRow {
        id,
        source_entry_id,
        source_sense_id,
        pos_id,
        base_form_id,
        form_id,
        variant_id,
        sense_id,
        dialect,
        form_type,
    } in components
    {
        candidates.push(Candidate {
            reference: reference(
                format!("phrase_component:{id}"),
                InboundReferenceKindV3::PhraseComponent,
                InboundReferenceTargetV3 {
                    pos_id,
                    base_form_id,
                    form_id,
                    variant_id,
                    sense_id,
                },
                InboundReferenceSourceV3 {
                    entry_id: Some(source_entry_id),
                    sense_id: source_sense_id,
                    node_id: Some(id),
                    ..InboundReferenceSourceV3::default()
                },
            ),
            rule: Rule::PhraseComponent {
                dialect: dialect.as_deref().and_then(parse_dialect),
                form_type: form_type.unwrap_or_default(),
            },
        });
    }
    Ok(candidates)
}

/// 按给定内容判定每条引用是否成立，并给只指向词义的引用补上所在词性。
fn evaluate(
    candidates: &mut [Candidate],
    entry_id: Uuid,
    forms: &DraftFormsStepContentV3,
    meanings: &DraftMeaningsStepContentV3,
) {
    let sense_pos = meanings
        .pos
        .iter()
        .flat_map(|pos| pos.senses.iter().map(move |sense| (sense.id, pos.pos_id)))
        .collect::<HashMap<_, _>>();
    // 成分用词与短语保存走同一判定函数，它吃的是 ComponentTargetWord；只在确有成分引用时才克隆内容。
    let component_target = candidates
        .iter()
        .any(|candidate| matches!(candidate.rule, Rule::PhraseComponent { .. }))
        .then(|| ComponentTargetWord {
            id: entry_id,
            kind: WordEntryKindV3::Word,
            label: String::new(),
            forms: forms.clone(),
            meanings: meanings.clone(),
            scope: ComponentTargetScope::Draft { revision: 0 },
        });
    for candidate in candidates {
        let target = &mut candidate.reference.target;
        let holds = match &candidate.rule {
            Rule::SharedSentence { link, dialect } => link.as_ref().is_some_and(|link| {
                super::text_links::shared_target_matches(forms, meanings, link, dialect)
            }),
            Rule::Sense => {
                let pos_id = target
                    .sense_id
                    .and_then(|sense_id| sense_pos.get(&sense_id).copied());
                target.pos_id = target.pos_id.or(pos_id);
                pos_id.is_some()
            }
            Rule::PhraseComponent { dialect, form_type } => {
                match (
                    component_target.as_ref(),
                    target.pos_id,
                    target.base_form_id,
                    target.sense_id,
                    target.form_id,
                    target.variant_id,
                    dialect,
                ) {
                    (
                        Some(word),
                        Some(pos_id),
                        Some(base_form_id),
                        Some(sense_id),
                        Some(form_id),
                        Some(variant_id),
                        Some(dialect),
                    ) => phrase_component_matches_target(
                        word,
                        pos_id,
                        base_form_id,
                        sense_id,
                        form_id,
                        variant_id,
                        *dialect,
                        form_type.clone(),
                        None,
                    ),
                    _ => false,
                }
            }
        };
        candidate.reference.stale = !holds;
    }
}

const fn kind_rank(kind: InboundReferenceKindV3) -> u8 {
    match kind {
        InboundReferenceKindV3::SharedSentence => 0,
        InboundReferenceKindV3::PublicationSenseRef => 1,
        InboundReferenceKindV3::DraftRelation => 2,
        InboundReferenceKindV3::PhraseComponent => 3,
        InboundReferenceKindV3::FormGroupSenseBinding => 4,
    }
}

/// 失效项优先（顶部提示依赖它们不被截断），再按类型与稳定 id。
fn sort_references(references: &mut [InboundReferenceV3]) {
    references.sort_by(|left, right| {
        right
            .stale
            .cmp(&left.stale)
            .then(kind_rank(left.kind).cmp(&kind_rank(right.kind)))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn node_counts(references: &[InboundReferenceV3]) -> Vec<InboundReferenceNodeV3> {
    let mut counts = BTreeMap::<Uuid, (InboundReferenceNodeTypeV3, u32)>::new();
    for reference in references {
        let target = &reference.target;
        let mut seen = HashSet::new();
        for (node_id, node_type) in [
            (target.pos_id, InboundReferenceNodeTypeV3::Pos),
            (target.base_form_id, InboundReferenceNodeTypeV3::Form),
            (target.form_id, InboundReferenceNodeTypeV3::Form),
            (target.variant_id, InboundReferenceNodeTypeV3::Variant),
            (target.sense_id, InboundReferenceNodeTypeV3::Sense),
        ] {
            if let Some(node_id) = node_id
                && seen.insert(node_id)
            {
                let count = counts.entry(node_id).or_insert((node_type, 0));
                count.1 = count.1.saturating_add(1);
            }
        }
    }
    counts
        .into_iter()
        .map(|(node_id, (node_type, total))| InboundReferenceNodeV3 {
            node_id,
            node_type,
            total,
        })
        .collect()
}

/// 草稿词义的第一条中文释义；按 JSON 取值，个别来源草稿形状异常时只缺摘要不报错。
fn sense_gloss(meanings: &Value, sense_id: Uuid) -> Option<String> {
    let sense_id = sense_id.to_string();
    meanings["pos"]
        .as_array()?
        .iter()
        .filter_map(|pos| pos["senses"].as_array())
        .flatten()
        .find(|sense| sense["id"].as_str() == Some(sense_id.as_str()))?["definitions"]
        .as_array()?
        .iter()
        .find(|definition| {
            matches!(
                definition["definition_mode"].as_str(),
                Some("zh_definition" | "zh_sentence")
            )
        })?["content"]["text"]
        .as_str()
        .map(str::to_owned)
}

async fn describe_sources(
    tx: &mut Transaction<'_, Postgres>,
    references: &mut [InboundReferenceV3],
) -> Result<(), LexiconServiceError> {
    let mut entry_ids = references
        .iter()
        .filter_map(|reference| reference.source.entry_id)
        .collect::<Vec<_>>();
    entry_ids.sort_unstable();
    entry_ids.dedup();
    if entry_ids.is_empty() {
        return Ok(());
    }
    let rows = sqlx::query_as::<_, (Uuid, String, String, bool, bool, Option<Value>)>(
        r#"
        SELECT entry.id, entry.kind, COALESCE(presentation.label, '') AS label,
               entry.archived_at IS NOT NULL AS is_archived,
               entry.current_publication_id IS NOT NULL AS is_published,
               projection.meanings
        FROM lexicon.entries entry
        LEFT JOIN lexicon.entry_presentation_projection presentation
          ON presentation.entry_id = entry.id
         AND presentation.content_schema_version = 3
        LEFT JOIN lexicon.entry_editor_projection projection
          ON projection.entry_id = entry.id
        WHERE entry.id = ANY($1)
        "#,
    )
    .bind(&entry_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    let entries = rows
        .into_iter()
        .map(|(id, kind, label, is_archived, is_published, meanings)| {
            let status = if is_archived {
                AdminWordStatus::Archived
            } else if is_published {
                AdminWordStatus::Published
            } else {
                AdminWordStatus::Draft
            };
            (id, (kind, label, status, meanings))
        })
        .collect::<HashMap<_, _>>();
    for reference in references {
        let source = &mut reference.source;
        let Some((kind, label, status, meanings)) =
            source.entry_id.and_then(|entry_id| entries.get(&entry_id))
        else {
            continue;
        };
        source.entry_headword = Some(label.clone());
        source.entry_kind = match kind.as_str() {
            "word" => Some(WordEntryKindV3::Word),
            "phrase" => Some(WordEntryKindV3::Phrase),
            _ => None,
        };
        source.entry_status = Some(*status);
        source.sense_gloss = source
            .sense_id
            .zip(meanings.as_ref())
            .and_then(|(sense_id, meanings)| sense_gloss(meanings, sense_id));
    }
    Ok(())
}

/// 给定内容下会被破坏（或已失效）的入站引用，带来源摘要，最多 500 条。
///
/// `baseline` 是已保存内容：草稿保存与词形影响预览只拦本次改动破坏的引用（基线里成立、提交后
/// 失效）。基线里本来就失效的旧引用不挡草稿保存——关联词可以指向只在发布版里还有的词义，来源
/// 词条归档期间目标删掉节点、之后来源恢复，都会造出目标编辑者自己解不开的失效引用。读接口照常
/// 标出这些引用；发布与切换版本不传基线，按现状拦。
pub(super) async fn inbound_reference_violations(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    forms: &DraftFormsStepContentV3,
    meanings: &DraftMeaningsStepContentV3,
    check: InboundReferenceCheck,
    baseline: Option<(&DraftFormsStepContentV3, &DraftMeaningsStepContentV3)>,
) -> Result<Vec<InboundReferenceV3>, LexiconServiceError> {
    let retained_sense_ids = meanings
        .pos
        .iter()
        .flat_map(|pos| pos.senses.iter().map(|sense| sense.id))
        .collect::<Vec<_>>();
    let mut candidates = collect_candidates(tx, entry_id, Some(&retained_sense_ids), check).await?;
    // 判定会就地补全 target 并改写 stale，基线要在副本上判，不能和提交内容共用一份。
    let already_stale = match baseline {
        Some((baseline_forms, baseline_meanings)) => {
            let mut before = candidates.clone();
            evaluate(&mut before, entry_id, baseline_forms, baseline_meanings);
            before
                .into_iter()
                .filter(|candidate| candidate.reference.stale)
                .map(|candidate| candidate.reference.id)
                .collect::<HashSet<_>>()
        }
        None => HashSet::new(),
    };
    evaluate(&mut candidates, entry_id, forms, meanings);
    let mut violations = candidates
        .into_iter()
        .map(|candidate| candidate.reference)
        .filter(|reference| reference.stale && !already_stale.contains(&reference.id))
        .collect::<Vec<_>>();
    sort_references(&mut violations);
    violations.truncate(MAX_INBOUND_REFERENCE_ITEMS);
    describe_sources(tx, &mut violations).await?;
    Ok(violations)
}

pub(super) async fn ensure_inbound_references(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    forms: &DraftFormsStepContentV3,
    meanings: &DraftMeaningsStepContentV3,
    check: InboundReferenceCheck,
    baseline: Option<(&DraftFormsStepContentV3, &DraftMeaningsStepContentV3)>,
) -> Result<(), LexiconServiceError> {
    let violations =
        inbound_reference_violations(tx, entry_id, forms, meanings, check, baseline).await?;
    if violations.is_empty() {
        Ok(())
    } else {
        Err(LexiconServiceError::InboundReferenceConflict(violations))
    }
}

impl LexiconService {
    /// 指向本词条当前草稿节点的全部入站引用，并标出已失效的。
    pub async fn inbound_references_v3(
        &self,
        entry_id: Uuid,
    ) -> Result<InboundReferencesV3, LexiconServiceError> {
        let word = self.get_v3(entry_id).await?;
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let mut candidates =
            collect_candidates(&mut tx, entry_id, None, InboundReferenceCheck::DraftPreview)
                .await?;
        evaluate(&mut candidates, entry_id, &word.forms, &word.meanings);
        let mut items = candidates
            .into_iter()
            .map(|candidate| candidate.reference)
            .collect::<Vec<_>>();
        let nodes = node_counts(&items);
        sort_references(&mut items);
        let truncated = items.len() > MAX_INBOUND_REFERENCE_ITEMS;
        items.truncate(MAX_INBOUND_REFERENCE_ITEMS);
        describe_sources(&mut tx, &mut items).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(InboundReferencesV3 {
            entry_id,
            revision: word.revision,
            nodes,
            items,
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(ids: [Option<Uuid>; 5]) -> InboundReferenceV3 {
        let [pos_id, base_form_id, form_id, variant_id, sense_id] = ids;
        reference(
            Uuid::now_v7().to_string(),
            InboundReferenceKindV3::SharedSentence,
            InboundReferenceTargetV3 {
                pos_id,
                base_form_id,
                form_id,
                variant_id,
                sense_id,
            },
            InboundReferenceSourceV3::default(),
        )
    }

    #[test]
    fn node_counts_protect_base_form_once_per_reference() {
        let [pos, base, derived, variant, sense] = [(); 5].map(|()| Uuid::now_v7());
        let counts = node_counts(&[
            target([
                Some(pos),
                Some(base),
                Some(base),
                Some(variant),
                Some(sense),
            ]),
            target([Some(pos), Some(base), Some(derived), None, None]),
        ]);
        let total = |id| {
            counts
                .iter()
                .find(|node| node.node_id == id)
                .map(|node| (node.node_type, node.total))
        };
        assert_eq!(total(pos), Some((InboundReferenceNodeTypeV3::Pos, 2)));
        assert_eq!(total(base), Some((InboundReferenceNodeTypeV3::Form, 2)));
        assert_eq!(total(derived), Some((InboundReferenceNodeTypeV3::Form, 1)));
        assert_eq!(
            total(variant),
            Some((InboundReferenceNodeTypeV3::Variant, 1))
        );
        assert_eq!(total(sense), Some((InboundReferenceNodeTypeV3::Sense, 1)));
    }

    #[test]
    fn stale_references_sort_first_then_by_kind() {
        let mut fresh = target([None; 5]);
        fresh.kind = InboundReferenceKindV3::SharedSentence;
        let mut stale = target([None; 5]);
        stale.kind = InboundReferenceKindV3::PhraseComponent;
        stale.stale = true;
        let mut references = vec![fresh, stale];
        sort_references(&mut references);
        assert!(references[0].stale);
    }

    #[test]
    fn sense_gloss_reads_first_chinese_definition() {
        let sense = Uuid::now_v7();
        let meanings = serde_json::json!({"pos":[{"senses":[{"id":sense,"definitions":[
            {"definition_mode":"en_definition","content":{"text":"calm"}},
            {"definition_mode":"zh_definition","content":{"text":"平静的"}}
        ]}]}]});
        assert_eq!(sense_gloss(&meanings, sense).as_deref(), Some("平静的"));
        assert_eq!(sense_gloss(&meanings, Uuid::now_v7()), None);
    }
}
