//! Explicit allow-list projection: never serialize AdminWordV3 to a wordlist reader.
use super::dto::*;
use crate::{error::AppError, lexicon::dto::*};
use std::collections::HashMap;
use uuid::Uuid;

pub struct ReadingLabels {
    pub forms: HashMap<String, String>,
    pub pos: HashMap<String, String>,
    pub sub_pos: HashMap<(String, String), String>,
}

pub fn project(
    entry_id: Uuid,
    publication_id: Uuid,
    snapshot: serde_json::Value,
    form_labels: Option<&ReadingLabels>,
) -> Result<WordlistEntry, AppError> {
    let word: AdminWordV3 = serde_json::from_value(snapshot).map_err(AppError::internal)?;
    if word.id != entry_id {
        return Err(AppError::internal(std::io::Error::other(
            "publication entry mismatch",
        )));
    }
    let pos = word
        .forms
        .pos
        .iter()
        .map(|form| {
            let meanings = word.meanings.pos.iter().find(|m| m.pos_id == form.pos_id);
            WordlistPos {
                pos_id: form.pos_id,
                pos: form.pos.clone(),
                label: form_labels.and_then(|labels| labels.pos.get(&form.pos).cloned()),
                forms: form_labels.map(|labels| {
                    form.forms
                        .iter()
                        .map(|item| WordlistForm {
                            id: item.id,
                            form_type: item.form_type.as_str().to_owned(),
                            label: labels
                                .forms
                                .get(item.form_type.as_str())
                                .cloned()
                                .unwrap_or_else(|| item.form_type.as_str().to_owned()),
                            sense_ids: crate::lexicon::published::allowed_form_senses(
                                form,
                                &word.meanings,
                                item.id,
                            )
                            .map(|sense| sense.id)
                            .collect(),
                            variants: match &item.regional_variants {
                                WordRegionalVariantsV3::Common { common } => vec![form_variant(
                                    common.id,
                                    Dialect::Common,
                                    &common.spelling,
                                    &common.pronunciations,
                                )],
                                WordRegionalVariantsV3::UkUs { uk, us } => vec![
                                    form_variant(
                                        uk.id,
                                        Dialect::Uk,
                                        &uk.spelling,
                                        &uk.pronunciations,
                                    ),
                                    form_variant(
                                        us.id,
                                        Dialect::Us,
                                        &us.spelling,
                                        &us.pronunciations,
                                    ),
                                ],
                            },
                        })
                        .collect()
                }),
                senses: meanings
                    .map(|m| {
                        m.senses
                            .iter()
                            .map(|sense| WordlistSense {
                                id: sense.id,
                                sub_pos_label: form_labels.and_then(|labels| {
                                    labels
                                        .sub_pos
                                        .get(&(form.pos.clone(), sense.sub_pos.clone()))
                                        .cloned()
                                }),
                                sub_pos: sense.sub_pos.clone(),
                                level: sense.level.clone(),
                                definitions: sense.definitions.iter().map(definition).collect(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                grammar_structures: meanings
                    .map(|m| {
                        m.grammar_structures
                            .iter()
                            .map(|g| WordlistGrammar {
                                id: g.id,
                                variants: g
                                    .variants
                                    .iter()
                                    .map(|v| WordlistText {
                                        dialect: v.dialect,
                                        content: v.content.clone(),
                                    })
                                    .collect(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect();
    Ok(WordlistEntry {
        entry_id,
        publication_id,
        label: word.presentation.label,
        kind: word.kind,
        pos,
    })
}
fn english(content: &EnglishTextV3) -> Vec<WordlistText> {
    match content {
        EnglishTextV3::Unified { common } => vec![WordlistText {
            dialect: Dialect::Common,
            content: common.value.clone(),
        }],
        EnglishTextV3::Distinguish { uk, us, .. } => [(Dialect::Uk, uk), (Dialect::Us, us)]
            .into_iter()
            .filter_map(|(dialect, slot)| match slot {
                DialectVariantRichTextSlotV3::Ready { variant } => Some(WordlistText {
                    dialect,
                    content: variant.value.clone(),
                }),
                DialectVariantRichTextSlotV3::Missing => None,
            })
            .collect(),
    }
}
fn definition(value: &WordDefinitionV3) -> WordlistDefinition {
    let (id, level, grammar, mode, texts) = match value {
        WordDefinitionV3::ZhDefinition {
            id,
            level,
            grammar_structure_id,
            content,
            ..
        } => (
            id,
            level,
            grammar_structure_id,
            "zh_definition",
            vec![WordlistText {
                dialect: Dialect::Common,
                content: content.clone(),
            }],
        ),
        WordDefinitionV3::ZhSentence {
            id,
            level,
            grammar_structure_id,
            content,
            ..
        } => (
            id,
            level,
            grammar_structure_id,
            "zh_sentence",
            vec![WordlistText {
                dialect: Dialect::Common,
                content: content.clone(),
            }],
        ),
        WordDefinitionV3::EnDefinition {
            id,
            level,
            grammar_structure_id,
            content,
        } => (
            id,
            level,
            grammar_structure_id,
            "en_definition",
            english(content),
        ),
        WordDefinitionV3::EnSentence {
            id,
            level,
            grammar_structure_id,
            content,
        } => (
            id,
            level,
            grammar_structure_id,
            "en_sentence",
            english(content),
        ),
    };
    WordlistDefinition {
        id: *id,
        level: level.clone(),
        grammar_structure_id: *grammar,
        definition_mode: mode.into(),
        texts,
    }
}
pub fn candidate(entry: WordlistEntry) -> WordlistCandidate {
    let glosses = entry
        .pos
        .iter()
        .flat_map(|p| &p.senses)
        .flat_map(|s| &s.definitions)
        .filter(|d| d.definition_mode.starts_with("zh_"))
        .flat_map(|d| &d.texts)
        .map(|t| t.content.text().to_owned())
        .take(2)
        .collect();
    WordlistCandidate {
        entry_id: entry.entry_id,
        publication_id: entry.publication_id,
        label: entry.label,
        glosses,
    }
}

fn form_variant(
    id: Uuid,
    dialect: Dialect,
    spelling: &str,
    pronunciations: &[WordPronunciationV3],
) -> WordlistFormVariant {
    WordlistFormVariant {
        id,
        dialect,
        spelling: spelling.to_owned(),
        pronunciations: pronunciations
            .iter()
            .map(|p| WordlistPronunciation {
                id: p.id,
                dict_phonetic: p.dict_phonetic.clone(),
                dict_phonetic_rich: p.dict_phonetic_rich.clone(),
            })
            .collect(),
    }
}
