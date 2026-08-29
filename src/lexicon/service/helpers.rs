use super::*;

/// 并列展示的方言顺序：common 或管理员主词侧在前。
/// 与列表行 SQL（repository/query.rs 的 ORDER BY CASE）保持同一规则。
pub(super) fn ordered_headword_sides(headwords: &WordHeadwordsV2) -> Vec<(Dialect, &str)> {
    match headwords {
        WordHeadwordsV2::Unified { common } => vec![(Dialect::Common, common.as_str())],
        WordHeadwordsV2::Distinguish {
            uk,
            us,
            source_dialect,
        } => match source_dialect {
            SourceDialect::Uk => vec![(Dialect::Uk, uk.as_str()), (Dialect::Us, us.as_str())],
            SourceDialect::Us => vec![(Dialect::Us, us.as_str()), (Dialect::Uk, uk.as_str())],
        },
    }
}

pub(super) fn normalize_submitted_headwords(
    headwords: &mut WordHeadwordsV2,
) -> Result<(), LexiconServiceError> {
    match headwords {
        WordHeadwordsV2::Unified { common } => {
            *common = NormalizedHeadword::parse(common)
                .map_err(map_headword_error)?
                .display;
        }
        WordHeadwordsV2::Distinguish { uk, us, .. } => {
            *uk = NormalizedHeadword::parse(uk)
                .map_err(map_headword_error)?
                .display;
            *us = NormalizedHeadword::parse(us)
                .map_err(map_headword_error)?
                .display;
        }
    }
    Ok(())
}

pub(super) fn relation_target_entry_ids(meanings: &DraftMeaningsStepContent) -> Vec<Uuid> {
    let mut entry_ids = meanings
        .pos
        .iter()
        .flat_map(|pos| pos.senses.iter())
        .flat_map(|sense| sense.relations.iter())
        // 待物化的关联词还没有目标词条，自然也没有需要一起锁的上下文。
        .filter_map(|relation| relation.target_word_id)
        .collect::<Vec<_>>();
    entry_ids.sort_unstable();
    entry_ids.dedup();
    entry_ids
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

pub(super) fn parse_v3_kind(value: &str) -> Option<WordEntryKindV3> {
    match value {
        "word" => Some(WordEntryKindV3::Word),
        "phrase" => Some(WordEntryKindV3::Phrase),
        _ => None,
    }
}

pub(super) const fn v3_kind_string(kind: WordEntryKindV3) -> &'static str {
    match kind {
        WordEntryKindV3::Word => "word",
        WordEntryKindV3::Phrase => "phrase",
    }
}

pub(super) const fn entry_kind_from_v3(kind: WordEntryKindV3) -> EntryKind {
    match kind {
        WordEntryKindV3::Word => EntryKind::Word,
        WordEntryKindV3::Phrase => EntryKind::Phrase,
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
            HeadwordNormalizationError::UnsupportedCharacter => {
                "headword must contain only Latin letters, digits and - ' . & / , characters"
            }
            HeadwordNormalizationError::MissingLatinLetter => {
                "headword must contain at least one Latin letter"
            }
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
        LexiconRepositoryError::ReferenceTargetChanged => LexiconServiceError::ReferenceConflict,
        LexiconRepositoryError::SurfaceContextBusy => LexiconServiceError::ReferenceConflict,
        other => LexiconServiceError::Repository(other),
    }
}

pub(super) fn database_error(error: sqlx::Error) -> LexiconServiceError {
    repository_error(LexiconRepositoryError::Database(error))
}

pub(super) fn serialization_error(error: serde_json::Error) -> LexiconServiceError {
    LexiconServiceError::Repository(LexiconRepositoryError::Serialization(error))
}

/// Existing projection/association writers are intentionally V2-only until their C2 V3 mapping
/// exists. Check the snapshot discriminator first so a V3/unknown snapshot fails closed with the
/// public unsupported-schema error instead of surfacing as an opaque serialization failure.
pub(super) fn v2_publication_snapshot(
    snapshot: serde_json::Value,
) -> Result<AdminWordV2, LexiconServiceError> {
    let version = snapshot
        .get("schema_version")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i16::try_from(value).ok())
        .unwrap_or(-1);
    if version != 2 {
        return Err(LexiconServiceError::UnsupportedSchemaVersion(version));
    }
    serde_json::from_value(snapshot).map_err(serialization_error)
}

pub(super) fn invariant_record() -> LexiconServiceError {
    LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
        "stored entry shape is invalid",
    ))
}
