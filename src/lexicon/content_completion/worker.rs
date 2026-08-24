use std::{sync::Arc, time::Duration};
use uuid::Uuid;

use crate::lexicon::{
    content_completion::{
        dto::{
            ContentCompletionDictionaryProvenance, ContentCompletionEvidenceKind,
            ContentCompletionFieldOrigins, ContentCompletionGenerationProvenance,
            ContentCompletionProvenance,
        },
        provider::{ContentGenerationSource, GeneratedPosContent, LexiconContentGenerator},
        repository::{ClaimedPartition, ContentCompletionRepository, PartitionResult},
    },
    dto::{
        Dialect, DialectVariantSlotV2, EnglishTextV2, GrammarStructureV2, GrammarVariantV2,
        RichText, RichTextV1, SenseGroupV2, SentenceAssociationsStateV2, TextOrigin, TextVariantV2,
        WordDefinitionV2, WordHeadwordsV2, WordPosMeaningsV2, WordSenseV2, WordSentenceLinkV2,
        WordSentenceV2,
    },
};

pub fn run_worker(pool: sqlx::PgPool, generator: Arc<dyn LexiconContentGenerator>) {
    tokio::spawn(async move {
        let repository = ContentCompletionRepository::new(pool);
        loop {
            match repository.claim().await {
                Ok(Some(partition)) => {
                    process_partition(&repository, generator.as_ref(), partition).await
                }
                Ok(None) => tokio::time::sleep(Duration::from_secs(1)).await,
                Err(error) => {
                    tracing::error!(error = %error, "content completion worker claim failed");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    });
}

async fn process_partition(
    repository: &ContentCompletionRepository,
    generator: &dyn LexiconContentGenerator,
    partition: ClaimedPartition,
) {
    let forms_pos = partition
        .source
        .forms
        .pos
        .iter()
        .find(|item| item.pos_id == partition.pos_id);
    let Some(forms_pos) = forms_pos else {
        let _ = repository
            .fail_partition(
                partition.job_id,
                partition.pos_id,
                partition.attempt,
                "source_not_found",
                "词性已不在任务来源快照中",
            )
            .await;
        return;
    };
    let dialect_mode = forms_pos.dialect_rules.spelling_mode.clone();
    let (dictionary_evidence, source_record_keys) =
        dictionary_evidence_for_pos(&partition.source.dictionary_evidence_by_pos, &partition.pos);
    if !has_dictionary_content(&dictionary_evidence) {
        let _ = repository
            .mark_partition_missing(
                partition.job_id,
                partition.pos_id,
                partition.attempt,
                "source_not_found",
                "当前 Kaikki 内容数据集没有该词性的释义正文",
            )
            .await;
        return;
    }
    let source = ContentGenerationSource {
        entry_id: partition.source.entry_id,
        pos_id: partition.pos_id,
        headword: partition.source.headword.clone(),
        pos: partition.pos.clone(),
        dialect_mode,
        dictionary_provider: partition.source.dictionary_provider.clone(),
        dictionary_version: partition.source.dictionary_version.clone(),
        source_record_keys: source_record_keys.clone(),
        dictionary_evidence,
    };
    match generator.generate(&source).await.and_then(|generated| {
        map_generated(&partition, generated).map_err(|_| {
            crate::lexicon::content_completion::provider::ContentGeneratorError::InvalidOutput
        })
    }) {
        Ok(result) => {
            let provenance = ContentCompletionProvenance {
                dictionary: ContentCompletionDictionaryProvenance {
                    provider: partition.source.dictionary_provider,
                    dataset_version: partition.source.dictionary_version,
                    source_record_keys,
                },
                generation: ContentCompletionGenerationProvenance {
                    provider: generator.provider_name().to_owned(),
                    model: generator.model().to_owned(),
                    prompt_version: "lexicon-content-v1".to_owned(),
                },
                field_origins: ContentCompletionFieldOrigins {
                    grammar_structures: ContentCompletionEvidenceKind::ModelInferred,
                    meanings: ContentCompletionEvidenceKind::DictionaryGroundedTranslation,
                    examples: ContentCompletionEvidenceKind::ModelGenerated,
                    cefr: ContentCompletionEvidenceKind::ModelInferred,
                },
                generated_at: chrono::Utc::now(),
            };
            if let Err(error) = repository
                .complete_partition(
                    partition.job_id,
                    partition.pos_id,
                    partition.attempt,
                    &result,
                    &provenance,
                )
                .await
            {
                tracing::error!(job_id=%partition.job_id, pos_id=%partition.pos_id, error=%error, "content completion result persistence failed");
            }
        }
        Err(error) => {
            if let Err(store_error) = repository
                .fail_partition(
                    partition.job_id,
                    partition.pos_id,
                    partition.attempt,
                    error.code(),
                    &error.to_string(),
                )
                .await
            {
                tracing::error!(job_id=%partition.job_id, pos_id=%partition.pos_id, error=%store_error, "content completion failure persistence failed");
            }
        }
    }
}

fn map_generated(
    partition: &ClaimedPartition,
    generated: GeneratedPosContent,
) -> Result<PartitionResult, ()> {
    if generated.grammar_structures.is_empty()
        || generated.grammar_structures.len() > 6
        || generated.senses.is_empty()
        || generated.senses.len() > 5
    {
        return Err(());
    }
    // 语法结构恒产单份 common，不看词条是不是 distinguish（提案 P1-b）。
    // 方言区分只属于 L1 词条事实（centre / center 这类地区拼写）；语法结构属于 L3——
    // 平台自己写的英文行文，只维护一份。此前对 distinguish 词条产 uk/us 双份同值镜像，
    // wire 里是纯冗余，学习端读到会显示成「英式：a centre／美式：a centre」这种没有信息量的两行。
    // P1 已放宽校验让 distinguish 词条同时接受 [common] 与 [uk, us]，所以这里改产单份是合法的。
    // 例句仍按词条事实分方言（见 english_text），本项不涉及。
    let grammar_structures = generated
        .grammar_structures
        .iter()
        .map(|grammar| {
            // 逐候选取第一个合法文本。顺序 common → uk → us：被迫二选一时偏英式，
            // 与 admins.dialect_preference 的默认值一致（迁移注释：默认英式，存量账号
            // 一并按英式解释）。逐个 filter 而不是先 or 再 filter——后者在 common 为空串
            // 时会让整个分区失败，即便 uk / us 里有完全可用的文本。
            let common = [
                grammar.common.as_deref(),
                grammar.uk.as_deref(),
                grammar.us.as_deref(),
            ]
            .into_iter()
            .flatten()
            .find(|value| valid_text(value, 500))
            .ok_or(())?;
            Ok(GrammarStructureV2 {
                id: Uuid::now_v7(),
                variants: vec![grammar_variant(Dialect::Common, common)],
            })
        })
        .collect::<Result<Vec<_>, ()>>()?;
    let mut groups = Vec::with_capacity(generated.senses.len());
    let mut senses = Vec::with_capacity(generated.senses.len());
    for generated_sense in generated.senses {
        if !valid_level(&generated_sense.level)
            || !valid_text(&generated_sense.group_name_zh, 120)
            || !valid_text(&generated_sense.group_name_en, 120)
            || generated_sense.sub_pos.chars().count() > 64
            || generated_sense.definitions.is_empty()
            || generated_sense.definitions.len() > 3
            || generated_sense.examples.len() < 2
            || generated_sense.examples.len() > 3
        {
            return Err(());
        }
        let group_id = Uuid::now_v7();
        groups.push(SenseGroupV2 {
            id: group_id,
            name_zh: generated_sense.group_name_zh,
            name_en: generated_sense.group_name_en,
        });
        let sense_id = Uuid::now_v7();
        let mut definitions = Vec::new();
        for definition in generated_sense.definitions {
            if !valid_level(&definition.level)
                || !valid_text(&definition.zh, 2000)
                || !valid_text(&definition.en, 2000)
            {
                return Err(());
            }
            let grammar_structure_id = match definition.grammar_index {
                Some(index) => Some(grammar_structures.get(index).ok_or(())?.id),
                None => None,
            };
            definitions.push(WordDefinitionV2::ZhDefinition {
                id: Uuid::now_v7(),
                content_id: Uuid::now_v7(),
                level: definition.level.clone(),
                grammar_structure_id,
                content: plain_text(definition.zh),
            });
            definitions.push(WordDefinitionV2::EnDefinition {
                id: Uuid::now_v7(),
                level: definition.level,
                grammar_structure_id,
                content: english_text(&partition.source.headwords, definition.en),
            });
        }
        let mut sentences = Vec::new();
        for example in generated_sense.examples {
            if !valid_level(&example.level)
                || !valid_text(&example.en, 2000)
                || !valid_text(&example.zh, 2000)
            {
                return Err(());
            }
            sentences.push(WordSentenceV2 {
                id: Uuid::now_v7(),
                level: example.level,
                en_text: english_text(&partition.source.headwords, example.en),
                zh_text_id: Uuid::now_v7(),
                zh_text: plain_text(example.zh),
                links: vec![WordSentenceLinkV2 {
                    word_id: partition.source.entry_id,
                    sense_id,
                    role: "focus".to_owned(),
                }],
                associations: Vec::new(),
                associations_state: SentenceAssociationsStateV2::Unresolved,
            });
        }
        senses.push(WordSenseV2 {
            id: sense_id,
            sub_pos: generated_sense.sub_pos,
            level: generated_sense.level,
            sense_group_id: Some(group_id),
            frequency: None,
            depends_on_context: generated_sense.depends_on_context,
            definitions,
            sentences,
            relations: Vec::new(),
        });
    }
    Ok(PartitionResult {
        sense_groups: groups,
        pos: WordPosMeaningsV2 {
            pos_id: partition.pos_id,
            grammar_structures,
            senses,
        },
    })
}

fn grammar_variant(dialect: Dialect, value: &str) -> GrammarVariantV2 {
    GrammarVariantV2 {
        id: Uuid::now_v7(),
        dialect,
        content: plain_text(value.to_owned()),
    }
}

fn plain_text(text: String) -> RichText {
    RichText::V1(RichTextV1 {
        version: 1,
        text,
        spans: Vec::new(),
        liaisons: Vec::new(),
    })
}

fn english_text(headwords: &WordHeadwordsV2, value: String) -> EnglishTextV2 {
    match headwords {
        WordHeadwordsV2::Unified { .. } => EnglishTextV2::Unified {
            common: text_variant(value),
        },
        WordHeadwordsV2::Distinguish { source_dialect, .. } => {
            let ready = |text: String| DialectVariantSlotV2::Ready {
                variant: text_variant(text),
            };
            EnglishTextV2::Distinguish {
                source_dialect: *source_dialect,
                uk: ready(value.clone()),
                us: ready(value),
            }
        }
    }
}

fn text_variant(value: String) -> TextVariantV2<RichText> {
    TextVariantV2 {
        id: Uuid::now_v7(),
        value: plain_text(value),
        origin: TextOrigin::Converted,
    }
}

fn valid_level(value: &str) -> bool {
    matches!(value, "A1" | "A2" | "B1" | "B2" | "C1" | "C2")
}

fn valid_text(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= max_chars
}

fn dictionary_evidence_for_pos(
    evidence: &std::collections::HashMap<String, serde_json::Value>,
    pos: &str,
) -> (serde_json::Value, Vec<String>) {
    let source_pos = match pos {
        "adjective" => "adj",
        "adverb" => "adv",
        "preposition" => "prep",
        "pronoun" => "pron",
        "conjunction" => "conj",
        "determiner" => "det",
        "interjection" => "intj",
        "numeral" => "num",
        "proper_noun" => "name",
        value => value,
    };
    let term = evidence.get("_term").cloned();
    let content = evidence
        .get(source_pos)
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let mut source_record_keys = term
        .as_ref()
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|record| record["normalized_term"].as_str())
        .map(|term| format!("dictionary.active_terms:{term}"))
        .collect::<Vec<_>>();
    source_record_keys.extend(
        content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|record| record["source_key"].as_str().map(str::to_owned)),
    );
    (
        serde_json::json!({"term": term, "content": content}),
        source_record_keys,
    )
}

fn has_dictionary_content(evidence: &serde_json::Value) -> bool {
    evidence["content"]
        .as_array()
        .is_some_and(|records| !records.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::{
        content_completion::provider::{
            GeneratedDefinition, GeneratedExample, GeneratedGrammar, GeneratedSense,
        },
        dto::{
            DialectRulesV2, DraftFormsStepContent, SourceDialect, WordBaseFormSlotV2,
            WordPosFormsV2,
        },
    };

    fn partition() -> ClaimedPartition {
        let pos_id = Uuid::now_v7();
        ClaimedPartition {
            job_id: Uuid::now_v7(),
            pos_id,
            pos: "noun".into(),
            attempt: 1,
            source: crate::lexicon::content_completion::repository::CompletionSourceSnapshot {
                entry_id: Uuid::now_v7(),
                headword: "workspace".into(),
                headwords: WordHeadwordsV2::Unified {
                    common: "workspace".into(),
                },
                forms: DraftFormsStepContent {
                    pos: vec![WordPosFormsV2 {
                        pos_id,
                        pos: "noun".into(),
                        dialect_rules: DialectRulesV2 {
                            spelling_mode: "unified".into(),
                            phonetic_mode: "unified".into(),
                        },
                        base_form: WordBaseFormSlotV2 {
                            id: Uuid::now_v7(),
                            form_type: "base".into(),
                            variants: vec![],
                        },
                        form_groups: vec![],
                    }],
                },
                dictionary_provider: "Kaikki".into(),
                dictionary_version: "test".into(),
                source_record_keys: vec!["key".into()],
                dictionary_evidence_by_pos: std::collections::HashMap::from([
                    (
                        "_term".to_owned(),
                        serde_json::json!([{
                            "normalized_term": "workspace",
                            "parts_of_speech": ["noun"]
                        }]),
                    ),
                    (
                        "noun".to_owned(),
                        serde_json::json!([{
                            "source_key": "key",
                            "senses": [{"glosses": ["an area used for work"]}]
                        }]),
                    ),
                ]),
            },
        }
    }

    /// distinguish 词条的分区夹具：词条事实分英美（centre / center），
    /// 但语法结构按 P1-b 仍应收敛成单份。
    fn distinguish_partition() -> ClaimedPartition {
        let mut partition = partition();
        partition.source.headword = "centre".into();
        for pos in &mut partition.source.forms.pos {
            pos.dialect_rules.spelling_mode = "distinguish".into();
            pos.dialect_rules.phonetic_mode = "distinguish".into();
        }
        partition.source.headwords = WordHeadwordsV2::Distinguish {
            uk: "centre".into(),
            us: "center".into(),
            source_dialect: SourceDialect::Uk,
        };
        partition
    }

    fn generated() -> GeneratedPosContent {
        GeneratedPosContent {
            grammar_structures: vec![GeneratedGrammar {
                common: Some("a workspace for something".into()),
                uk: None,
                us: None,
            }],
            senses: vec![GeneratedSense {
                group_name_zh: "工作空间".into(),
                group_name_en: "work area".into(),
                sub_pos: "".into(),
                level: "B1".into(),
                depends_on_context: false,
                definitions: vec![GeneratedDefinition {
                    zh: "用于工作的空间".into(),
                    en: "an area used for work".into(),
                    level: "B1".into(),
                    grammar_index: Some(0),
                }],
                examples: vec![
                    GeneratedExample {
                        en: "Keep your workspace tidy.".into(),
                        zh: "保持工作空间整洁。".into(),
                        level: "A2".into(),
                    },
                    GeneratedExample {
                        en: "The app opens a new workspace.".into(),
                        zh: "应用会打开新的工作区。".into(),
                        level: "B1".into(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn mapped_content_has_closed_references_and_multiple_examples() {
        let partition = partition();
        let result = map_generated(&partition, generated()).unwrap();
        let sense = &result.pos.senses[0];
        assert_eq!(sense.sentences.len(), 2);
        assert_eq!(sense.sentences[0].links[0].sense_id, sense.id);
        let WordDefinitionV2::ZhDefinition {
            grammar_structure_id,
            ..
        } = &sense.definitions[0]
        else {
            panic!()
        };
        assert_eq!(
            *grammar_structure_id,
            Some(result.pos.grammar_structures[0].id)
        );
    }

    #[test]
    fn invalid_cefr_or_dangling_grammar_is_rejected() {
        let partition = partition();
        let mut value = generated();
        value.senses[0].level = "Z9".into();
        assert!(map_generated(&partition, value).is_err());
        let mut value = generated();
        value.senses[0].definitions[0].grammar_index = Some(9);
        assert!(map_generated(&partition, value).is_err());
        let mut value = generated();
        value.senses[0].examples[0].en = "x".repeat(2001);
        assert!(map_generated(&partition, value).is_err());
    }

    #[test]
    fn more_than_three_definitions_are_rejected() {
        let partition = partition();
        let mut value = generated();
        let definition = value.senses[0].definitions[0].clone();
        value.senses[0].definitions = vec![definition; 4];
        assert!(map_generated(&partition, value).is_err());
    }

    #[test]
    fn missing_dictionary_content_is_not_treated_as_generation_evidence() {
        let partition = partition();
        let (evidence, _) =
            dictionary_evidence_for_pos(&partition.source.dictionary_evidence_by_pos, "verb");
        assert!(!has_dictionary_content(&evidence));
    }

    // P1-b：语法结构属于 L3（平台自己写的英文行文），恒产单份 common。
    // distinguish 词条此前会拿到 uk/us 双份同值镜像——wire 冗余，且学习端会显示成
    // 「英式：a centre／美式：a centre」这种没有信息量的两行。
    #[test]
    fn grammar_structures_always_collapse_to_a_single_common_variant() {
        let distinguish = distinguish_partition();

        // 模型只给 common：不再镜像成两份。
        let result = map_generated(&distinguish, generated()).unwrap();
        let variants = &result.pos.grammar_structures[0].variants;
        assert_eq!(
            variants.len(),
            1,
            "distinguish 词条的语法结构应收敛为单条，实际 {variants:#?}"
        );
        assert_eq!(variants[0].dialect, Dialect::Common);

        // 模型即便返回了英美两侧文本，也仍然只落一条 common——
        // 提示词已禁止编造方言差异，这里再钉一道，避免镜像从模型侧漏回来。
        // 被迫二选一时取英式，与 admins.dialect_preference 的默认值一致。
        let mut dialect_specific = generated();
        dialect_specific.grammar_structures = vec![GeneratedGrammar {
            common: None,
            uk: Some("a centre for something".into()),
            us: Some("a center for something".into()),
        }];
        let result = map_generated(&distinguish, dialect_specific).unwrap();
        let variants = &result.pos.grammar_structures[0].variants;
        assert_eq!(variants.len(), 1, "模型给了双侧也不应产出两条");
        assert_eq!(variants[0].dialect, Dialect::Common);
        assert_eq!(
            variants[0].content.text(),
            "a centre for something",
            "缺 common 时应取英式，不是美式"
        );

        // common 存在但非法（空白串）时要回退到下一个合法候选，而不是让整个分区失败。
        // 两条一起钉住回退顺序：uk 在场时取 uk，uk 缺席才轮到 us。
        // 只留后者的话，把顺序写反成 common → us → uk 也照样通过。
        for (uk, us, expected) in [
            (
                Some("a centre for something"),
                Some("a center for something"),
                "a centre for something",
            ),
            (
                None,
                Some("a center for something"),
                "a center for something",
            ),
        ] {
            let mut empty_common = generated();
            empty_common.grammar_structures = vec![GeneratedGrammar {
                common: Some("   ".into()),
                uk: uk.map(Into::into),
                us: us.map(Into::into),
            }];
            let result = map_generated(&distinguish, empty_common).unwrap();
            assert_eq!(
                result.pos.grammar_structures[0].variants[0].content.text(),
                expected,
                "common 非法时应按 uk → us 回退，而不是整条分区失败"
            );
        }

        // unified 词条的行为不受本次改动影响。
        let result = map_generated(&partition(), generated()).unwrap();
        assert_eq!(result.pos.grammar_structures[0].variants.len(), 1);
    }

    #[test]
    fn provenance_keys_are_limited_to_the_partition_evidence() {
        let mut partition = partition();
        partition.source.dictionary_evidence_by_pos.insert(
            "verb".into(),
            serde_json::json!([{"source_key": "verb-key", "senses": []}]),
        );
        let (_, keys) =
            dictionary_evidence_for_pos(&partition.source.dictionary_evidence_by_pos, "noun");
        assert_eq!(keys, vec!["dictionary.active_terms:workspace", "key"]);
    }
}
