//! Codepoint token boundaries shared by voice-editor annotation validation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodepointRange {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) range: CodepointRange,
    pub(crate) hard_boundary_before: bool,
}

pub(crate) fn tokenize(text: &str) -> Vec<Token> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut token_ranges = Vec::<CodepointRange>::new();
    let mut current_start = None;
    for (index, character) in characters.iter().copied().enumerate() {
        if is_token_character(character) {
            current_start.get_or_insert(index);
        } else if let Some(start) = current_start.take() {
            push_trimmed_token_range(&characters, start, index, &mut token_ranges);
        }
    }
    if let Some(start) = current_start {
        push_trimmed_token_range(&characters, start, characters.len(), &mut token_ranges);
    }
    let mut previous_end = 0;
    token_ranges
        .into_iter()
        .map(|range| {
            let hard_boundary_before = characters[previous_end..range.start]
                .iter()
                .copied()
                .any(is_hard_boundary);
            previous_end = range.end;
            Token {
                range,
                hard_boundary_before,
            }
        })
        .collect()
}

fn is_token_character(character: char) -> bool {
    character.is_alphanumeric() || is_connector(character) || is_combining_mark(character)
}

fn is_connector(character: char) -> bool {
    matches!(
        character,
        '\'' | '\u{2018}' | '\u{2019}' | '\u{02bc}' | '-' | '\u{2010}' | '\u{2011}'
    )
}

fn is_combining_mark(character: char) -> bool {
    matches!(character, '\u{0300}'..='\u{036f}' | '\u{1ab0}'..='\u{1aff}' | '\u{1dc0}'..='\u{1dff}' | '\u{20d0}'..='\u{20ff}' | '\u{fe20}'..='\u{fe2f}')
}

fn is_hard_boundary(character: char) -> bool {
    matches!(
        character,
        '.' | '!' | '?' | ';' | ':' | '\n' | '\r' | '。' | '！' | '？' | '；' | '：'
    )
}

fn push_trimmed_token_range(
    characters: &[char],
    mut start: usize,
    mut end: usize,
    target: &mut Vec<CodepointRange>,
) {
    while start < end && is_connector(characters[start]) {
        start += 1;
    }
    while end > start && is_connector(characters[end - 1]) {
        end -= 1;
    }
    if start < end
        && characters[start..end]
            .iter()
            .any(|character| character.is_alphanumeric())
    {
        target.push(CodepointRange { start, end });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_keep_codepoint_boundaries_and_connectors() {
        let text = "👩 'don't' re-enter cafe\u{301}. Next";
        let chars = text.chars().collect::<Vec<_>>();
        let tokens = tokenize(text);
        let words = tokens
            .iter()
            .map(|t| chars[t.range.start..t.range.end].iter().collect::<String>())
            .collect::<Vec<_>>();
        assert_eq!(words, ["don't", "re-enter", "cafe\u{301}", "Next"]);
        assert_eq!(tokens[0].range.start, 3);
        assert!(!tokens[2].hard_boundary_before);
        assert!(tokens[3].hard_boundary_before);
        assert!(tokenize("--- ''").is_empty());
    }

    #[test]
    fn line_breaks_and_sentence_punctuation_are_hard_boundaries() {
        for boundary in ["\n", ".", "!", "?", ";", ":", "。", "！", "？"] {
            let tokens = tokenize(&format!("dress{boundary}up"));
            assert_eq!(tokens.len(), 2);
            assert!(tokens[1].hard_boundary_before);
        }
        assert!(!tokenize("dress, up")[1].hard_boundary_before);
    }
}
