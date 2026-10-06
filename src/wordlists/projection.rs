//! Explicit allow-list projection: never serialize AdminWordV3 to a wordlist reader.
use super::dto::*;
use crate::{error::AppError, lexicon::dto::*};
use uuid::Uuid;

pub fn project(
    entry_id: Uuid,
    publication_id: Uuid,
    snapshot: serde_json::Value,
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
                senses: meanings
                    .map(|m| {
                        m.senses
                            .iter()
                            .map(|sense| WordlistSense {
                                id: sense.id,
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
