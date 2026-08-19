use super::*;

pub(super) fn compatible_headwords(
    detected: &WordHeadwordsV2,
    matched_dialect: Dialect,
    submitted: &WordHeadwordsV2,
) -> Result<bool, LexiconServiceError> {
    let detected_source = match detected {
        WordHeadwordsV2::Distinguish { source_dialect, .. } => Some(*source_dialect),
        WordHeadwordsV2::Unified { .. } => match matched_dialect {
            Dialect::Uk => Some(SourceDialect::Uk),
            Dialect::Us => Some(SourceDialect::Us),
            Dialect::Common => None,
        },
    };
    if let (Some(detected_source), WordHeadwordsV2::Distinguish { source_dialect, .. }) =
        (detected_source, submitted)
        && detected_source != *source_dialect
    {
        return Ok(false);
    }
    Ok(normalize_headword(source_headword(detected))
        .map_err(map_headword_error)?
        .key
        == normalize_headword(source_headword(submitted))
            .map_err(map_headword_error)?
            .key)
}

fn source_headword(headwords: &WordHeadwordsV2) -> &str {
    match headwords {
        WordHeadwordsV2::Unified { common } => common,
        WordHeadwordsV2::Distinguish {
            uk,
            us,
            source_dialect,
        } => match source_dialect {
            SourceDialect::Uk => uk,
            SourceDialect::Us => us,
        },
    }
}

pub(super) fn normalize_submitted_headwords(
    headwords: &mut WordHeadwordsV2,
) -> Result<(), LexiconServiceError> {
    match headwords {
        WordHeadwordsV2::Unified { common } => {
            *common = normalize_headword(common)
                .map_err(map_headword_error)?
                .display;
        }
        WordHeadwordsV2::Distinguish { uk, us, .. } => {
            *uk = normalize_headword(uk).map_err(map_headword_error)?.display;
            *us = normalize_headword(us).map_err(map_headword_error)?.display;
        }
    }
    Ok(())
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

pub(super) fn parse_source_dialect(value: &str) -> Option<SourceDialect> {
    match value {
        "uk" => Some(SourceDialect::Uk),
        "us" => Some(SourceDialect::Us),
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

pub(super) fn surface_projection_error(_error: HeadwordNormalizationError) -> LexiconServiceError {
    LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
        "persisted surface normalization failed",
    ))
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
