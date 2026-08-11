use std::{cmp::Ordering, collections::BTreeMap};

use crate::lexicon::dto::{
    DialectVariantSlotV2, DraftMeaningsStepContent, EnglishTextV2, RichText, RichTextAnnotation,
    RichTextV1, RichTextV2, WordDefinitionV2,
};

pub const MAX_RICH_TEXT_CODEPOINTS: usize = 5_000;
pub const MAX_RICH_TEXT_ANNOTATIONS: usize = 500;
pub const MAX_PAUSE_MS: u32 = 5_000;
pub const MAX_PHONEME_CODEPOINTS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichTextIssue {
    pub code: &'static str,
    pub path: String,
    pub message: &'static str,
}

mod core;

pub use core::{canonicalize, canonicalize_meanings, is_valid};

#[cfg(test)]
mod tests;
