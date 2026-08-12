use crate::lexicon::{
    dto::{Dialect, SourceDialect, TextOrigin, WordHeadwordsV2},
    normalization::normalize_headword,
};

pub(crate) fn headword_origin(
    detected: &WordHeadwordsV2,
    matched_dialect: Dialect,
    submitted: &WordHeadwordsV2,
    dialect: Dialect,
    spelling: &str,
) -> TextOrigin {
    let dictionary_value = match (detected, submitted, dialect) {
        (
            WordHeadwordsV2::Unified { common: detected },
            WordHeadwordsV2::Unified { .. },
            Dialect::Common,
        ) => Some(detected.as_str()),
        (
            WordHeadwordsV2::Unified { common: detected },
            WordHeadwordsV2::Distinguish { source_dialect, .. },
            Dialect::Uk,
        ) if locked_dialect(matched_dialect, *source_dialect) == SourceDialect::Uk => {
            Some(detected.as_str())
        }
        (
            WordHeadwordsV2::Unified { common: detected },
            WordHeadwordsV2::Distinguish { source_dialect, .. },
            Dialect::Us,
        ) if locked_dialect(matched_dialect, *source_dialect) == SourceDialect::Us => {
            Some(detected.as_str())
        }
        (
            WordHeadwordsV2::Distinguish { uk, .. },
            WordHeadwordsV2::Distinguish { .. },
            Dialect::Uk,
        ) => Some(uk.as_str()),
        (
            WordHeadwordsV2::Distinguish { us, .. },
            WordHeadwordsV2::Distinguish { .. },
            Dialect::Us,
        ) => Some(us.as_str()),
        (
            WordHeadwordsV2::Distinguish {
                uk,
                us,
                source_dialect,
            },
            WordHeadwordsV2::Unified { .. },
            Dialect::Common,
        ) => Some(match source_dialect {
            SourceDialect::Uk => uk.as_str(),
            SourceDialect::Us => us.as_str(),
        }),
        _ => None,
    };

    if dictionary_value.is_some_and(|value| same_headword(value, spelling)) {
        TextOrigin::Dictionary
    } else {
        TextOrigin::Manual
    }
}

fn locked_dialect(matched_dialect: Dialect, submitted_source: SourceDialect) -> SourceDialect {
    match matched_dialect {
        Dialect::Uk => SourceDialect::Uk,
        Dialect::Us => SourceDialect::Us,
        Dialect::Common => submitted_source,
    }
}

fn same_headword(left: &str, right: &str) -> bool {
    match (normalize_headword(left), normalize_headword(right)) {
        (Ok(left), Ok(right)) => left.key == right.key,
        _ => false,
    }
}
