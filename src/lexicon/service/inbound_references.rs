//! 本词条节点的入站引用：多维例句标注、其他词条当前发布版本里的词义引用、草稿关联词、短语成分用词。
//! 影响预览与发布共用完整目标判定；草稿允许编辑，发布前必须处理破坏性影响。

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
        InboundReferencesV3, PhraseComponentUsageV3, TextLinkV3,
    },
    model::InboundSenseReferenceRecord,
    shared_sentences::SentenceTarget,
};

const MAX_INBOUND_REFERENCE_ITEMS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InboundReferenceCheck {
    /// 草稿影响预览：收集全部活跃来源，只读不加锁；不阻止保存。
    DraftPreview,
    /// 发布与切换版本：验证全部活跃来源，破坏性影响必须先修复。
    PublicationContent,
}

impl InboundReferenceCheck {
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
    ViaPhrase {
        link: Box<TextLinkV3>,
    },
    TextLink {
        link: Box<TextLinkV3>,
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

fn text_link_candidates(
    candidate: &Candidate,
    link: &TextLinkV3,
    target_entry_id: Uuid,
    sense_id: Option<Uuid>,
) -> Vec<Candidate> {
    let mut expanded = Vec::new();
    if link.target_word_id == target_entry_id
        && sense_id.is_none_or(|id| id == link.target_sense_id)
    {
        let mut direct = candidate.clone();
        direct.reference.id = format!("{}:{}:direct", direct.reference.id, link.id);
        direct.reference.target = InboundReferenceTargetV3 {
            pos_id: Some(link.target_pos_id),
            base_form_id: Some(link.target_base_form_id),
            form_id: Some(link.target_form_id),
            variant_id: Some(link.target_variant_id),
            sense_id: Some(link.target_sense_id),
        };
        direct.rule = Rule::TextLink {
            link: Box::new(link.clone()),
        };
        expanded.push(direct);
    }
    if let Some(phrase) = &link.via_phrase
        && phrase.word_id == target_entry_id
        && sense_id.is_none_or(|id| id == phrase.sense_id)
    {
        let mut via = candidate.clone();
        via.reference.id = format!("{}:{}:via", via.reference.id, link.id);
        via.reference.target.sense_id = Some(phrase.sense_id);
        via.rule = Rule::ViaPhrase {
            link: Box::new(link.clone()),
        };
        expanded.push(via);
    }
    expanded
}

/// 发布索引只存词义锚点；完整目标从同一不可变来源快照恢复，不能把成分/正文链接降级为词义检查。
fn expand_publication_candidate(
    candidate: Candidate,
    word: &AdminWordV3,
    target_entry_id: Uuid,
) -> Result<Vec<Candidate>, LexiconServiceError> {
    let node_id = candidate
        .reference
        .source
        .node_id
        .ok_or_else(invariant_record)?;
    let sense_id = candidate
        .reference
        .target
        .sense_id
        .ok_or_else(invariant_record)?;
    match candidate.reference.source.reference_kind.as_deref() {
        Some("relation" | "sentence_context") => Ok(vec![candidate]),
        Some("phrase_component") => {
            for usage in super::v3_publication::all_component_usages(&word.forms, &word.meanings) {
                if let PhraseComponentUsageV3::Resolved {
                    id,
                    target_word_id,
                    target_pos_id,
                    target_base_form_id,
                    target_form_id,
                    target_variant_id,
                    target_sense_id,
                    target_dialect,
                    target_form_type,
                    ..
                } = usage
                    && id == node_id
                    && target_word_id == target_entry_id
                    && target_sense_id == sense_id
                {
                    let mut candidate = candidate;
                    candidate.reference.target = InboundReferenceTargetV3 {
                        pos_id: Some(target_pos_id),
                        base_form_id: Some(target_base_form_id),
                        form_id: Some(target_form_id),
                        variant_id: Some(target_variant_id),
                        sense_id: Some(target_sense_id),
                    };
                    candidate.rule = Rule::PhraseComponent {
                        dialect: Some(target_dialect),
                        form_type: target_form_type,
                    };
                    return Ok(vec![candidate]);
                }
            }
            Err(invariant_record())
        }
        Some("text_link") => {
            let variant = super::text_links::variants(&word.meanings)
                .find(|variant| variant.id == node_id)
                .ok_or_else(invariant_record)?;
            let expanded = variant
                .text_links
                .iter()
                .flat_map(|link| {
                    text_link_candidates(&candidate, link, target_entry_id, Some(sense_id))
                })
                .collect::<Vec<_>>();
            if expanded.is_empty() {
                return Err(invariant_record());
            }
            Ok(expanded)
        }
        _ => Err(invariant_record()),
    }
}

/// 单独发布的出站目标必须在当前发布内容中成立；与入站影响复用相同完整身份判定。
/// 固定历史快照仍保留供回放，但不能替代当前目标的有效性校验。
pub(super) async fn outbound_publication_issues(
    tx: &mut Transaction<'_, Postgres>,
    word: &AdminWordV3,
) -> Result<Vec<DraftValidationIssue>, LexiconServiceError> {
    let mut keys = std::collections::BTreeSet::new();
    for sense in word.meanings.pos.iter().flat_map(|pos| &pos.senses) {
        for relation in &sense.relations {
            if let Some((entry, target_sense)) =
                relation.target_word_id.zip(relation.target_sense_id)
            {
                keys.insert((relation.id, "relation", entry, target_sense));
            }
        }
        for sentence in &sense.sentences {
            for link in &sentence.links {
                if link.role == "context" && link.word_id != word.id {
                    keys.insert((sentence.id, "sentence_context", link.word_id, link.sense_id));
                }
            }
        }
    }
    for usage in super::v3_publication::all_component_usages(&word.forms, &word.meanings) {
        if let PhraseComponentUsageV3::Resolved {
            id,
            target_word_id,
            target_sense_id,
            ..
        } = usage
        {
            keys.insert((id, "phrase_component", target_word_id, target_sense_id));
        }
    }
    for variant in super::text_links::variants(&word.meanings) {
        for link in &variant.text_links {
            keys.insert((
                variant.id,
                "text_link",
                link.target_word_id,
                link.target_sense_id,
            ));
            if let Some(via) = &link.via_phrase {
                keys.insert((variant.id, "text_link", via.word_id, via.sense_id));
            }
        }
    }
    let target_ids = keys
        .iter()
        .map(|key| key.2)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let targets = LexiconRepository::current_publication_snapshots(tx, &target_ids)
        .await
        .map_err(repository_error)?
        .into_iter()
        .map(|record| {
            serde_json::from_value::<AdminWordV3>(record.snapshot)
                .map(|word| (record.entry_id, word))
                .map_err(serialization_error)
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    let mut issues = Vec::new();
    for (node_id, kind, entry_id, sense_id) in keys {
        let candidate = Candidate {
            reference: reference(
                format!("outbound:{node_id}:{kind}:{sense_id}"),
                InboundReferenceKindV3::PublicationSenseRef,
                InboundReferenceTargetV3 {
                    sense_id: Some(sense_id),
                    ..InboundReferenceTargetV3::default()
                },
                InboundReferenceSourceV3 {
                    node_id: Some(node_id),
                    reference_kind: Some(kind.to_owned()),
                    ..InboundReferenceSourceV3::default()
                },
            ),
            rule: Rule::Sense,
        };
        let mut candidates = expand_publication_candidate(candidate, word, entry_id)?;
        if let Some(target) = targets.get(&entry_id) {
            evaluate(&mut candidates, entry_id, &target.forms, &target.meanings);
            if candidates.iter().all(|item| !item.reference.stale) {
                continue;
            }
        }
        let (field, code) = match kind {
            "relation" => ("target_sense_id", "relation_target_unavailable"),
            "sentence_context" => ("links", "sentence_context_target_unavailable"),
            "phrase_component" => ("component_usages", "phrase_component_target_unavailable"),
            _ => ("text_links", "definition_invalid"),
        };
        let in_meanings = kind != "phrase_component" || word.meanings.pos.iter().flat_map(|pos| &pos.senses)
            .flat_map(|sense| &sense.component_usages).any(|usage| matches!(usage, PhraseComponentUsageV3::Resolved { id, .. } if *id == node_id));
        issues.push(DraftValidationIssue {
            step: if in_meanings {
                PersistedWordStep::Meanings
            } else {
                PersistedWordStep::Forms
            },
            node_id,
            field: field.to_owned(),
            code: code.to_owned(),
            message: "具体引用目标尚未发布或已不在当前发布内容中，请先发布目标或修复引用"
                .to_owned(),
            reference_location: None,
            node_location: None,
        });
    }
    Ok(issues)
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
    let publication_refs = match check.locks() {
        true => LexiconRepository::current_inbound_sense_refs(tx, entry_id, &[])
            .await
            .map_err(repository_error)?,
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
            ORDER BY sense_ref.target_sense_id, sense_ref.entry_id, sense_ref.source_node_id
            "#,
        )
        .bind(entry_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)?,
    };
    let publication_ids = publication_refs
        .iter()
        .map(|record| record.source_publication_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let snapshots = sqlx::query_as::<_, (Uuid, Value)>(
        "SELECT id, snapshot FROM lexicon.entry_publications WHERE id = ANY($1)",
    )
    .bind(&publication_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?
    .into_iter()
    .map(|(id, snapshot)| {
        serde_json::from_value::<AdminWordV3>(snapshot)
            .map(|word| (id, word))
            .map_err(serialization_error)
    })
    .collect::<Result<HashMap<_, _>, _>>()?;
    for record in publication_refs {
        let word = snapshots
            .get(&record.source_publication_id)
            .ok_or_else(invariant_record)?;
        let candidate = Candidate {
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
        };
        candidates.extend(expand_publication_candidate(candidate, word, entry_id)?);
    }

    // 内部绑定属于当前草稿，仅参与草稿影响预览。发布/历史激活不改变草稿，
    // 不能用候选发布快照判断这些内部绑定是否失效；外部引用仍完整检查。
    let bindings = sqlx::query_as::<_, (Uuid, Uuid, Uuid, i32)>(
        r#"
        WITH bindings AS (
            SELECT sense_id, form_group_id, entry_pos_id FROM lexicon.sense_form_group_bindings WHERE entry_id = $1
            UNION
            SELECT id, form_group_id, entry_pos_id FROM lexicon.senses WHERE entry_id = $1 AND form_group_id IS NOT NULL
        )
        SELECT binding.sense_id, binding.form_group_id, binding.entry_pos_id, form_group.ordinal
        FROM bindings binding JOIN lexicon.v3_form_groups form_group ON form_group.id = binding.form_group_id
        WHERE $2
        ORDER BY binding.sense_id, binding.form_group_id
        "#,
    )
    .bind(entry_id)
    .bind(check == InboundReferenceCheck::DraftPreview)
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
    // 固定快照保护历史回放，但不能隐藏当前活跃草稿的影响；发布时也要报告这些依赖。
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
          AND usage.state = 'resolved'
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
          AND usage.state = 'resolved'
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
    // 正文草稿没有独立引用表；从当前编辑投影读取，保留各自业务结构，不建立第二套校验器。
    let texts = sqlx::query_as::<_, (Uuid, Value)>(r#"
        SELECT entry.id, editor.meanings
        FROM lexicon.entries entry
        JOIN lexicon.entry_editor_projection editor ON editor.entry_id = entry.id
        WHERE entry.archived_at IS NULL AND entry.id <> $1
          AND (jsonb_path_exists(editor.meanings, '$.**.target_word_id ? (@ == $target)', jsonb_build_object('target', $1::text))
            OR jsonb_path_exists(editor.meanings, '$.**.via_phrase.word_id ? (@ == $target)', jsonb_build_object('target', $1::text)))
        ORDER BY entry.id
    "#).bind(entry_id).fetch_all(&mut **tx).await.map_err(database_error)?;
    for (source_entry_id, meanings) in texts {
        let meanings = serde_json::from_value::<DraftMeaningsStepContentV3>(meanings)
            .map_err(serialization_error)?;
        for variant in super::text_links::variants(&meanings) {
            let candidate = Candidate {
                reference: reference(
                    format!("draft_text_link:{}:{}", source_entry_id, variant.id),
                    InboundReferenceKindV3::DraftTextLink,
                    InboundReferenceTargetV3::default(),
                    InboundReferenceSourceV3 {
                        entry_id: Some(source_entry_id),
                        node_id: Some(variant.id),
                        reference_kind: Some("text_link".to_owned()),
                        ..InboundReferenceSourceV3::default()
                    },
                ),
                rule: Rule::Sense,
            };
            for link in &variant.text_links {
                candidates.extend(text_link_candidates(&candidate, link, entry_id, None));
            }
        }
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
        .any(|candidate| {
            matches!(
                candidate.rule,
                Rule::PhraseComponent { .. } | Rule::ViaPhrase { .. }
            )
        })
        .then(|| ComponentTargetWord {
            id: entry_id,
            kind: WordEntryKindV3::Phrase,
            label: String::new(),
            forms: forms.clone(),
            meanings: meanings.clone(),
            scope: ComponentTargetScope::Draft { revision: 0 },
        });
    for candidate in candidates {
        let target = &mut candidate.reference.target;
        let holds = match &candidate.rule {
            Rule::TextLink { link } => {
                super::text_links::text_target_matches(forms, meanings, link)
            }
            Rule::ViaPhrase { link } => component_target
                .as_ref()
                .is_some_and(|word| super::text_links::component_matches(word, link)),
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
        InboundReferenceKindV3::DraftTextLink => 5,
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
/// 词形影响预览传已保存的 `baseline`，只提示本次新增影响；已有失效项由引用读接口展示。
/// 草稿保存不调用拦截。发布、发布前校验及切换版本不传基线，必须修复所有当前失效引用。
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
