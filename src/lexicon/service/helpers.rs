use super::*;

pub(super) fn compatible_headwords(
    detected: &WordHeadwordsV2,
    submitted: &WordHeadwordsV2,
) -> Result<bool, LexiconServiceError> {
    Ok(match (detected, submitted) {
        (
            WordHeadwordsV2::Unified { common: expected },
            WordHeadwordsV2::Unified { common: actual },
        ) => {
            normalize_headword(expected)
                .map_err(map_headword_error)?
                .key
                == normalize_headword(actual).map_err(map_headword_error)?.key
        }
        (
            WordHeadwordsV2::Distinguish {
                uk: expected_uk,
                us: expected_us,
                source_dialect,
            },
            WordHeadwordsV2::Distinguish {
                uk,
                us,
                source_dialect: submitted_source,
            },
        ) => {
            if source_dialect != submitted_source {
                false
            } else {
                let expected = if *source_dialect == SourceDialect::Uk {
                    expected_uk
                } else {
                    expected_us
                };
                let actual = if *source_dialect == SourceDialect::Uk {
                    uk
                } else {
                    us
                };
                normalize_headword(expected)
                    .map_err(map_headword_error)?
                    .key
                    == normalize_headword(actual).map_err(map_headword_error)?.key
                    && !normalize_headword(uk)
                        .map_err(map_headword_error)?
                        .key
                        .is_empty()
                    && !normalize_headword(us)
                        .map_err(map_headword_error)?
                        .key
                        .is_empty()
            }
        }
        _ => false,
    })
}

pub(super) fn validate_headwords(headwords: &WordHeadwordsV2) -> Result<(), LexiconServiceError> {
    match headwords {
        WordHeadwordsV2::Unified { common } => {
            normalize_headword(common).map_err(map_headword_error)?;
        }
        WordHeadwordsV2::Distinguish { uk, us, .. } => {
            normalize_headword(uk).map_err(map_headword_error)?;
            normalize_headword(us).map_err(map_headword_error)?;
        }
    }
    Ok(())
}

pub(super) fn normalized_headword_keys(
    headwords: &WordHeadwordsV2,
) -> Result<Vec<String>, LexiconServiceError> {
    match headwords {
        WordHeadwordsV2::Unified { common } => Ok(vec![
            normalize_headword(common).map_err(map_headword_error)?.key,
        ]),
        WordHeadwordsV2::Distinguish { uk, us, .. } => Ok(vec![
            normalize_headword(uk).map_err(map_headword_error)?.key,
            normalize_headword(us).map_err(map_headword_error)?.key,
        ]),
    }
}

pub(super) fn map_dictionary_pos(values: &[String]) -> Vec<String> {
    let mut output = Vec::new();
    for value in values {
        let normalized = value.trim().to_ascii_lowercase();
        let mapped = match normalized.as_str() {
            "noun" | "name" | "proper noun" => Some("noun"),
            "pronoun" | "pron" => Some("pronoun"),
            "verb" | "auxiliary" | "modal" => Some("verb"),
            "adjective" | "adj" => Some("adjective"),
            "adverb" | "adv" => Some("adverb"),
            "preposition" | "prep" => Some("preposition"),
            "article" => Some("article"),
            "determiner" | "det" => Some("determiner"),
            "conjunction" | "conj" => Some("conjunction"),
            "numeral" | "number" | "num" => Some("numeral"),
            "interjection" | "intj" => Some("interjection"),
            _ => None,
        };
        if let Some(mapped) = mapped
            && !output.iter().any(|existing| existing == mapped)
        {
            output.push(mapped.to_owned());
        }
    }
    output
}

pub(super) fn family_dialect(family: &str) -> Option<Dialect> {
    match family {
        "british_core" | "british_influenced" => Some(Dialect::Uk),
        "american_core" | "american_influenced" => Some(Dialect::Us),
        _ => None,
    }
}

pub(super) fn parse_kind(value: &str) -> Option<EntryKind> {
    match value {
        "word" => Some(EntryKind::Word),
        "phrase" => Some(EntryKind::Phrase),
        _ => None,
    }
}

pub(super) fn parse_dialect(value: &str) -> Option<Dialect> {
    match value {
        "common" => Some(Dialect::Common),
        "uk" => Some(Dialect::Uk),
        "us" => Some(Dialect::Us),
        _ => None,
    }
}

pub(super) fn kind_string_owned(kind: EntryKind) -> String {
    match kind {
        EntryKind::Word => "word",
        EntryKind::Phrase => "phrase",
    }
    .to_owned()
}

pub(super) fn status_string_owned(status: AdminWordStatus) -> String {
    match status {
        AdminWordStatus::Draft => "draft",
        AdminWordStatus::Published => "published",
        AdminWordStatus::Archived => "archived",
    }
    .to_owned()
}

pub(super) fn max_reachable_step(completed: &[String]) -> WordCreationStep {
    if completed.iter().any(|step| step == "meanings") {
        WordCreationStep::Preview
    } else if completed.iter().any(|step| step == "forms") {
        WordCreationStep::Meanings
    } else {
        WordCreationStep::Forms
    }
}

pub(super) fn valid_level(value: &str) -> bool {
    matches!(value, "A1" | "A2" | "B1" | "B2" | "C1" | "C2")
}

pub(super) fn trimmed(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

pub(super) fn map_headword_error(error: HeadwordNormalizationError) -> LexiconServiceError {
    LexiconServiceError::InvalidField {
        field: "headword",
        message: match error {
            HeadwordNormalizationError::Empty => "headword is required",
            HeadwordNormalizationError::TooLong => "headword is too long",
            HeadwordNormalizationError::ControlCharacter => "headword contains control characters",
        },
    }
}

pub(super) fn repository_error(error: LexiconRepositoryError) -> LexiconServiceError {
    match error {
        LexiconRepositoryError::DuplicateHeadword => LexiconServiceError::DuplicateWord,
        LexiconRepositoryError::TargetPublicationBusy => LexiconServiceError::ReferenceConflict,
        other => LexiconServiceError::Repository(other),
    }
}

pub(super) fn database_error(error: sqlx::Error) -> LexiconServiceError {
    repository_error(LexiconRepositoryError::Database(error))
}

pub(super) fn serialization_error(error: serde_json::Error) -> LexiconServiceError {
    LexiconServiceError::Repository(LexiconRepositoryError::Serialization(error))
}

pub(super) fn invariant_record() -> LexiconServiceError {
    LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
        "stored entry shape is invalid",
    ))
}
