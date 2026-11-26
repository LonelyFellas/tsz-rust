//! Versioned adaptation of a reviewed publication to the existing student reader.
//! Source, form, sense and definition IDs always belong to the published document.
use super::{
    dialect_pairs::{NextDialectSense, NextDialectUsage},
    language,
    model::*,
    publication_model::NextPublicationSnapshot,
};
use crate::{error::AppError, lexicon::dto::*};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
pub(crate) struct SourceLabel {
    pub label: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct StudentDocument {
    pub id: Uuid,
    pub kind: WordEntryKindV3,
    pub presentation: SourceLabel,
    pub forms: DraftFormsStepContentV3,
    pub meanings: DraftMeaningsStepContentV3,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_equivalents: Vec<String>,
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub excluded_forms: HashSet<Uuid>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub sense_locales: HashMap<Uuid, NextDialectUsage>,
}

fn definition(row: &NextDefinition, medium: &str) -> Result<Option<WordDefinitionV3>, AppError> {
    let id = row.id;
    let level = row.level.clone().ok_or_else(language::invalid)?;
    let grammar_structure_id = row.grammar_structure_id;
    let text = row.content.clone().unwrap_or_else(|| {
        RichTextV3::V2(RichTextV2V3 {
            version: 2,
            text: row.text.clone(),
            annotations: vec![],
        })
    });
    let sentence = matches!(row.style, NextDefinitionStyle::Sentence);
    if row.locale == medium {
        return Ok(Some(if sentence {
            WordDefinitionV3::ZhSentence {
                id,
                content_id: id,
                level,
                grammar_structure_id,
                content: text,
            }
        } else {
            WordDefinitionV3::ZhDefinition {
                id,
                content_id: id,
                level,
                grammar_structure_id,
                content: text,
            }
        }));
    }
    let variant = RichTextVariantV3 {
        id,
        value: text,
        origin: TextOrigin::Manual,
        voice_profile: None,
        audio_assets: vec![],
        text_links: Default::default(),
        reading_links: Default::default(),
    };
    let content = match row.locale.as_str() {
        "en" => EnglishTextV3::Unified { common: variant },
        "en-GB" => EnglishTextV3::Distinguish {
            source_dialect: SourceDialect::Uk,
            uk: DialectVariantRichTextSlotV3::Ready {
                variant: Box::new(variant),
            },
            us: DialectVariantRichTextSlotV3::Missing,
        },
        "en-US" => EnglishTextV3::Distinguish {
            source_dialect: SourceDialect::Us,
            uk: DialectVariantRichTextSlotV3::Missing,
            us: DialectVariantRichTextSlotV3::Ready {
                variant: Box::new(variant),
            },
        },
        _ => return Ok(None),
    };
    Ok(Some(if sentence {
        WordDefinitionV3::EnSentence {
            id,
            level,
            grammar_structure_id,
            content,
        }
    } else {
        WordDefinitionV3::EnDefinition {
            id,
            level,
            grammar_structure_id,
            content,
        }
    }))
}

pub(crate) fn project(
    entry: Uuid,
    kind: WordEntryKindV3,
    medium: &str,
    pubdoc: NextPublicationSnapshot,
    dialects: &[NextDialectSense],
) -> Result<StudentDocument, AppError> {
    let draft = pubdoc.draft;
    let mut forms = draft
        .form_editor
        .ok_or_else(language::invalid)?
        .resolve()
        .map_err(|e| e.into_app_error())?;
    // Persist the resolved reader graph; the immutable authoring graph remains in lexicon_next.
    forms.shared = None;
    let mut meanings = DraftMeaningsStepContentV3::default();
    for pos in &forms.pos {
        let mut senses = vec![];
        for sense in draft
            .senses
            .iter()
            .filter(|s| s.part_of_speech.as_deref() == Some(pos.pos.as_str()))
        {
            let level = sense.level.clone().ok_or_else(language::invalid)?;
            let mut definitions = Vec::new();
            for row in &sense.definitions {
                if let Some(d) = definition(row, medium)? {
                    definitions.push(d);
                }
            }
            let relations = sense
                .relations
                .iter()
                .flatten()
                .map(|r| {
                    let target = pubdoc.relation_targets.iter().find(|t| {
                        Some(t.entry_id) == r.target_word_id
                            && Some(t.sense_id) == r.target_sense_id
                    });
                    WordRelationV3 {
                        id: r.id,
                        relation: r.relation.clone(),
                        target_word_id: r.target_word_id,
                        target_sense_id: r.target_sense_id,
                        pending_target_headword: r.pending_target_headword.clone(),
                        pending_target_gloss: r.pending_target_gloss.clone(),
                        target_headword: target.map(|t| t.headword.clone()),
                        target_gloss: target.map(|t| t.gloss.clone()),
                        target_status: None,
                        score: r.score.clone(),
                    }
                })
                .collect();
            senses.push(WordSenseV3 {
                id: sense.id,
                sub_pos: sense.sub_pos.clone().unwrap_or_default(),
                level,
                sense_group_id: sense.sense_group_id,
                form_group_id: None,
                form_group_ids: None,
                frequency: sense.frequency.clone(),
                depends_on_context: sense.depends_on_context.unwrap_or(false),
                definitions,
                sentences: vec![],
                relations,
                component_usages: Default::default(),
            });
        }
        let grammar_structures = draft
            .grammar_structures
            .iter()
            .flatten()
            .filter(|g| g.part_of_speech == pos.pos)
            .map(|g| GrammarStructureV3 {
                id: g.id,
                variants: g
                    .variants
                    .iter()
                    .map(|v| GrammarVariantV3 {
                        id: v.id,
                        dialect: match v.locale.as_str() {
                            "en-GB" => Dialect::Uk,
                            "en-US" => Dialect::Us,
                            _ => Dialect::Common,
                        },
                        content: v.content.clone(),
                        form_links: None,
                        voice_profile: None,
                        audio_assets: vec![],
                    })
                    .collect(),
            })
            .collect();
        meanings.pos.push(WordPosMeaningsV3 {
            pos_id: pos.pos_id,
            grammar_structures,
            senses,
        });
    }
    Ok(StudentDocument {
        id: entry,
        kind,
        presentation: SourceLabel {
            label: draft.headword,
        },
        forms,
        meanings,
        // Equivalents exclude ambiguous distractors. They do not supply a graded definition.
        known_equivalents: draft
            .senses
            .iter()
            .flat_map(|s| &s.equivalents)
            .filter(|e| e.locale == medium)
            .map(|e| e.text.trim().to_owned())
            .filter(|text| !text.is_empty())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        excluded_forms: draft
            .form_question_eligibility
            .iter()
            .filter(|s| !s.eligible)
            .map(|s| s.form_id)
            .collect(),
        sense_locales: dialects
            .iter()
            .filter(|d| d.usage != NextDialectUsage::General)
            .map(|d| (d.sense_id, d.usage))
            .collect(),
    })
}
impl StudentDocument {
    pub(crate) fn for_questions(
        mut self,
        profile: Option<&str>,
    ) -> crate::lexicon::published::PublishedContentV3 {
        if let Some(profile) = profile {
            for pos in &mut self.meanings.pos {
                pos.senses.retain(|s| match self.sense_locales.get(&s.id) {
                    Some(NextDialectUsage::British) => profile == "BrE",
                    Some(NextDialectUsage::American) => profile == "AmE",
                    _ => true,
                });
            }
        }
        crate::lexicon::published::PublishedContentV3 {
            excluded_form_ids: self.excluded_forms,
            forms: self.forms,
            meanings: self.meanings,
        }
    }
}
