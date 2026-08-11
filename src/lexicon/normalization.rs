use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

pub const HEADWORD_NORMALIZATION_VERSION: i16 = 1;
pub const MAX_HEADWORD_CODEPOINTS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHeadword {
    pub display: String,
    pub key: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HeadwordNormalizationError {
    #[error("headword is empty")]
    Empty,
    #[error("headword is too long")]
    TooLong,
    #[error("headword contains control characters")]
    ControlCharacter,
}

pub fn normalize_headword(value: &str) -> Result<NormalizedHeadword, HeadwordNormalizationError> {
    if value.chars().any(char::is_control) {
        return Err(HeadwordNormalizationError::ControlCharacter);
    }

    let display = collapse_whitespace(value.nfkc());
    if display.is_empty() {
        return Err(HeadwordNormalizationError::Empty);
    }
    if display.chars().count() > MAX_HEADWORD_CODEPOINTS {
        return Err(HeadwordNormalizationError::TooLong);
    }

    let key = display
        .chars()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{02bc}' => '\'',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            other => other,
        })
        .flat_map(char::to_lowercase)
        .collect();

    Ok(NormalizedHeadword { display, key })
}

pub fn sha256_json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let bytes = serde_json::to_vec(value)?;
    Ok(Sha256::digest(bytes).to_vec())
}

fn collapse_whitespace(characters: impl Iterator<Item = char>) -> String {
    let mut output = String::new();
    let mut pending_space = false;

    for character in characters {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(character);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_is_versioned_and_stable() {
        assert_eq!(HEADWORD_NORMALIZATION_VERSION, 1);
        assert_eq!(
            normalize_headword("  It\u{2019}s\u{3000}Well—Known  ").unwrap(),
            NormalizedHeadword {
                display: "It’s Well—Known".to_owned(),
                key: "it's well-known".to_owned(),
            }
        );
    }

    #[test]
    fn compatibility_characters_and_case_share_one_key() {
        assert_eq!(
            normalize_headword("ＣＥＮＴＥＲ").unwrap().key,
            normalize_headword("center").unwrap().key
        );
    }

    #[test]
    fn rejects_empty_long_and_control_values() {
        assert_eq!(
            normalize_headword(" \t ").unwrap_err(),
            HeadwordNormalizationError::ControlCharacter
        );
        assert_eq!(
            normalize_headword("   ").unwrap_err(),
            HeadwordNormalizationError::Empty
        );
        assert_eq!(
            normalize_headword(&"a".repeat(201)).unwrap_err(),
            HeadwordNormalizationError::TooLong
        );
    }
}
