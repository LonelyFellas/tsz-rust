//! Frozen text normalization mechanics. Domain validation and version selection
//! belong to callers; future rule changes must not alter these v1 functions.
use unicode_normalization::UnicodeNormalization;

pub(crate) fn display_v1(value: &str) -> String {
    let mut output = String::new();
    let mut pending_space = false;
    for character in value.nfkc() {
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

pub(crate) fn key_v1(value: &str) -> String {
    display_v1(value)
        .chars()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{02bc}' => '\'',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            other => other,
        })
        .flat_map(char::to_lowercase)
        .collect()
}
