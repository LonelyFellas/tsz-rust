use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::lexicon::dto::{
    Dialect, DialectVariantSlotV2, DraftFormsStepContent, DraftMeaningsStepContent,
    DraftValidationIssue, EnglishTextV2, PersistedWordStep, RichText, WordDefinitionV2,
    WordHeadwordsV2,
};
use crate::lexicon::model::NodeIdentityRecord;

mod helpers;
mod meanings;
mod structure;

use helpers::*;

pub use meanings::validate_meanings;
pub use structure::validate_forms;
pub(crate) use structure::{
    proposed_nodes, validate_node_identities, validate_node_limit, validate_persisted_text,
};

#[cfg(test)]
mod tests {
    use super::valid_percent;

    #[test]
    fn fixed_percent_is_decimal_and_bounded() {
        for valid in ["0", "0.01", "99.99", "100", "100.0", "100.00"] {
            assert!(valid_percent(valid), "{valid}");
        }
        for invalid in ["", "-1", "1.234", "100.01", "NaN", ".5", "1."] {
            assert!(!valid_percent(invalid), "{invalid}");
        }
    }
}
