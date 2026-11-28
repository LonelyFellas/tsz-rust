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
pub const DRAFT_SCHEMA_VERSION: i32 = 1;
pub(super) fn valid_pos(code: &str) -> bool {
    (POS.contains(&code) || code.starts_with("phrase_"))
        && crate::catalog::service::valid_part_code(code.to_owned()).is_ok()
}
pub fn capabilities() -> NextCapabilities {
    NextCapabilities {
        sense_component_usages: None,
        creation_snapshot: None,
        sentence_drafts: None,
        relations: None,
        dialect_pairs: None,
        sentence_details: None,
        definition_details: None,
        sentences: None,
        equivalent_metadata: None,
        sense_metadata: None,
        grammar_audio: None,
        grammar_structures: None,
        grammar_form_links: None,
        sentence_associations: None,
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
        generation_candidates: None,
        section_generation_candidates: None,
        dictionary_refresh: None,
        publishing: true,
        form_editor: None,
        form_question_eligibility: None,
        sense_groups: None,
    }
}
pub fn draft_schema_compatibility() -> NextDraftSchemaCompatibility {
    NextDraftSchemaCompatibility {
        draft_schema_version: DRAFT_SCHEMA_VERSION,
        supported_draft_schema_versions: vec![DRAFT_SCHEMA_VERSION],
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
    if draft.schema_version != DRAFT_SCHEMA_VERSION {
        return Err(AppError::unprocessable(
            ErrorCode::UnsupportedSchemaVersion,
            "schema_version must match the negotiated draft schema",
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
            .map(|s| {
                s.definitions.len()
                    + s.equivalents.len()
                    + s.relations.as_ref().map_or(0, Vec::len)
                    + s.component_usages.as_ref().map_or(0, Vec::len)
            })
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
    super::definitions::validate(draft)?;
    for form in &draft.forms {
        identity(&mut ids, form.id)?;
        if !matches!(form.locale.as_str(), "en" | "en-GB" | "en-US") {
            return Err(unavailable());
        }
        text(&form.text, 200)?;
    }
    super::relations::validate(draft, &mut ids)?;
    for sense in &draft.senses {
        identity(&mut ids, sense.id)?;
        super::components::validate(sense, &mut ids)?;
        if sense
            .level
            .as_deref()
            .is_some_and(|value| !crate::lexicon::validation::valid_level(value))
            || sense
                .frequency
                .as_deref()
                .is_some_and(|value| !crate::lexicon::validation::valid_percent(value))
        {
            return Err(invalid());
        }
        if sense
            .sense_group_id
            .is_some_and(|id| !group_ids.contains(&id))
        {
            return Err(invalid());
        }
        if sense
            .part_of_speech
            .as_deref()
            .is_some_and(|p| !valid_pos(p))
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
    preserve_invalid_bindings: bool,
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
                if !preserve_invalid_bindings
                    && definition.grammar_structure_id.is_some_and(|id| {
                        entry
                            .draft
                            .grammar_structures
                            .iter()
                            .flatten()
                            .find(|g| g.id == id)
                            .is_none_or(|g| !super::grammar::matches_sense(g, sense, parents))
                    })
                {
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
                component_usages: sense.component_usages.clone(),
                relations: sense.relations.clone(),
                sentence_ids: sense.sentence_ids.clone(),
                no_equivalents: Some(sense.no_equivalents.contains(&selection.medium_locale)),
                level: sense.level.clone(),
                frequency: sense.frequency.clone(),
                depends_on_context: sense.depends_on_context,
                id: sense.id,
                sense_group_id: sense.sense_group_id,
                missing_target_definitions: target_definitions.is_empty(),
                missing_medium_definitions: medium_definitions.is_empty(),
                missing_equivalents: equivalents.is_empty()
                    && !sense.no_equivalents.contains(&selection.medium_locale),
                target_definitions,
                medium_definitions,
                equivalents,
            }
        })
        .collect();
    Ok(NextEntryView {
        dialect_senses: None,
        sentence_resources: None,
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
