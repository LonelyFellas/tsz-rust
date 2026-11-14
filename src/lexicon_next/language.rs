use std::collections::HashSet;
use uuid::Uuid;

use super::model::*;
use crate::error::{AppError, ErrorCode};

pub(super) const POS: &[&str] = &[
    "noun",
    "verb",
    "adjective",
    "adverb",
    "pronoun",
    "determiner",
    "preposition",
    "conjunction",
    "interjection",
    "numeral",
    "particle",
];
pub fn capabilities() -> NextCapabilities {
    NextCapabilities {
        grammar_audio: None,
        grammar_structures: None,
        medium_ownership: None,
        schema_version: 1,
        language_rules_version: "lexicon-next-en-zh-1".into(),
        languages: vec![NextLanguageCapability {
            target_language: "en".into(),
            target_locales: vec!["en-GB".into(), "en-US".into()],
            medium_locales: vec!["zh-Hans".into(), "zh-Hant".into()],
            common_target_locale: "en".into(),
            part_of_speech_codes: POS.iter().map(|s| (*s).into()).collect(),
        }],
        manual_drafts: true,
        generation: false,
        publishing: false,
        form_editor: None,
        sense_groups: None,
    }
}
pub fn unavailable() -> AppError {
    AppError::unprocessable(
        ErrorCode::CapabilityUnavailable,
        "language or locale capability is not available",
    )
}
pub fn invalid() -> AppError {
    AppError::unprocessable(ErrorCode::InvalidRequestBody, "invalid lexicon-next draft")
}
pub fn require_language(language: &str) -> Result<(), AppError> {
    if language == "en" {
        Ok(())
    } else {
        Err(unavailable())
    }
}
pub fn require_medium(locale: &str) -> Result<(), AppError> {
    if matches!(locale, "zh-Hans" | "zh-Hant") {
        Ok(())
    } else {
        Err(unavailable())
    }
}
pub fn text(value: &str, max: usize) -> Result<(), AppError> {
    validate_text(value, max, false)
}
fn validate_text(value: &str, max: usize, multiline: bool) -> Result<(), AppError> {
    if value.trim().is_empty()
        || value.chars().count() > max
        || value
            .chars()
            .any(|c| c.is_control() && !(multiline && matches!(c, '\r' | '\n')))
    {
        Err(invalid())
    } else {
        Ok(())
    }
}
pub(super) fn identity(ids: &mut HashSet<Uuid>, id: Uuid) -> Result<(), AppError> {
    if id.is_nil() || !ids.insert(id) {
        Err(invalid())
    } else {
        Ok(())
    }
}

pub fn validate(language: &str, draft: &NextDraft) -> Result<(), AppError> {
    require_language(language)?;
    if draft.schema_version != 1 {
        return Err(AppError::unprocessable(
            ErrorCode::UnsupportedSchemaVersion,
            "schema_version must be 1",
        ));
    }
    text(&draft.headword, 200)?;
    // A bounded initial editing contract, not a content production quota.
    let count = draft
        .grammar_structures
        .as_ref()
        .map_or(0, |rows| rows.iter().map(|g| 1 + g.variants.len()).sum())
        + draft.sense_groups.as_ref().map_or(0, Vec::len)
        + draft.forms.len()
        + draft.senses.len()
        + draft
            .senses
            .iter()
            .map(|s| s.definitions.len() + s.equivalents.len())
            .sum::<usize>();
    if count > 500 {
        return Err(invalid());
    }
    let mut ids = HashSet::new();
    let groups = draft.sense_groups.as_deref().unwrap_or_default();
    let group_ids = groups.iter().map(|group| group.id).collect::<HashSet<_>>();
    for group in groups {
        identity(&mut ids, group.id)?;
        for name in [&group.name_zh, &group.name_en] {
            if name.chars().count() > 200 || name.chars().any(char::is_control) {
                return Err(invalid());
            }
        }
    }
    super::grammar::validate(draft, &mut ids)?;
    for form in &draft.forms {
        identity(&mut ids, form.id)?;
        if !matches!(form.locale.as_str(), "en" | "en-GB" | "en-US") {
            return Err(unavailable());
        }
        text(&form.text, 200)?;
    }
    for sense in &draft.senses {
        identity(&mut ids, sense.id)?;
        if sense
            .sense_group_id
            .is_some_and(|id| !group_ids.contains(&id))
        {
            return Err(invalid());
        }
        if sense
            .part_of_speech
            .as_deref()
            .is_some_and(|p| !POS.contains(&p))
        {
            return Err(invalid());
        }
        for definition in &sense.definitions {
            identity(&mut ids, definition.id)?;
            if !matches!(
                definition.locale.as_str(),
                "en" | "en-GB" | "en-US" | "zh-Hans" | "zh-Hant"
            ) {
                return Err(unavailable());
            }
            validate_text(&definition.text, 5000, true)?;
        }
        for equivalent in &sense.equivalents {
            identity(&mut ids, equivalent.id)?;
            if !matches!(equivalent.locale.as_str(), "zh-Hans" | "zh-Hant") {
                return Err(unavailable());
            }
            text(&equivalent.text, 200)?;
        }
    }
    if let Some(editor) = &draft.form_editor {
        super::forms::validate(editor, &mut ids)?;
    }
    Ok(())
}

pub fn view(
    entry: &NextEntry,
    selection: NextViewQuery,
    parents: &std::collections::HashMap<String, String>,
) -> Result<NextEntryView, AppError> {
    require_language(&entry.target_language)?;
    if !matches!(selection.target_locale.as_str(), "en-GB" | "en-US")
        || !matches!(selection.medium_locale.as_str(), "zh-Hans" | "zh-Hant")
    {
        return Err(unavailable());
    }
    let all_forms = super::forms::projection(&entry.draft)?;
    let form_locale = if all_forms
        .iter()
        .any(|f| f.locale == selection.target_locale)
    {
        &selection.target_locale
    } else {
        &entry.target_language
    };
    let forms: Vec<_> = all_forms
        .iter()
        .filter(|f| &f.locale == form_locale)
        .cloned()
        .collect();
    let senses = entry
        .draft
        .senses
        .iter()
        .map(|sense| {
            let target = if sense
                .definitions
                .iter()
                .any(|d| d.locale == selection.target_locale)
            {
                &selection.target_locale
            } else {
                &entry.target_language
            };
            let mut target_definitions: Vec<_> = sense
                .definitions
                .iter()
                .filter(|d| &d.locale == target)
                .cloned()
                .collect();
            let mut medium_definitions: Vec<_> = sense
                .definitions
                .iter()
                .filter(|d| d.locale == selection.medium_locale)
                .cloned()
                .collect();
            for definition in target_definitions
                .iter_mut()
                .chain(medium_definitions.iter_mut())
            {
                if definition.grammar_structure_id.is_some_and(|id| {
                    entry
                        .draft
                        .grammar_structures
                        .iter()
                        .flatten()
                        .find(|g| g.id == id)
                        .is_none_or(|g| !super::grammar::matches_sense(g, sense, parents))
                }) {
                    definition.grammar_structure_id = None;
                }
            }
            let equivalents: Vec<_> = sense
                .equivalents
                .iter()
                .filter(|d| d.locale == selection.medium_locale)
                .cloned()
                .collect();
            NextSenseView {
                id: sense.id,
                sense_group_id: sense.sense_group_id,
                missing_target_definitions: target_definitions.is_empty(),
                missing_medium_definitions: medium_definitions.is_empty(),
                missing_equivalents: equivalents.is_empty(),
                target_definitions,
                medium_definitions,
                equivalents,
            }
        })
        .collect();
    Ok(NextEntryView {
        grammar_structures: entry.draft.grammar_structures.as_ref().map(|rows| {
            rows.iter()
                .filter(|g| {
                    super::grammar::valid_subcategory(
                        &g.part_of_speech,
                        g.sub_pos.as_deref().unwrap_or(""),
                        parents,
                    )
                })
                .cloned()
                .map(|mut grammar| {
                    let exact = grammar
                        .variants
                        .iter()
                        .any(|v| v.locale == selection.target_locale);
                    grammar.variants.retain(|v| {
                        if exact {
                            v.locale == selection.target_locale
                        } else {
                            v.locale == "en"
                        }
                    });
                    grammar
                })
                .collect()
        }),
        grammar_issues: None,
        sense_groups: entry.draft.sense_groups.clone(),
        entry_id: entry.id,
        revision: entry.revision,
        target_locale: selection.target_locale,
        medium_locale: selection.medium_locale,
        missing_forms: forms.is_empty(),
        forms,
        senses,
    })
}
