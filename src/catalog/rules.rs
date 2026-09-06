//! 基础词性规则：细分词性只允许挂在固定的五个基础词性下。
//!
//! 这是产品固定口径而非可配置状态，所以不落库，由后端按 `code` 派生并通过
//! `sub_parts_extensible` 下发；前端只读该字段，不得复制这份编码集合。

/// 允许扩展细分词性的基础词性编码。
pub const BASIC_PART_OF_SPEECH_CODES: [&str; 5] =
    ["noun", "verb", "pronoun", "adjective", "adverb"];

/// 该基本词性是否允许挂细分词性。
pub fn is_basic_part_of_speech(code: &str) -> bool {
    BASIC_PART_OF_SPEECH_CODES.contains(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_five_basic_codes_allow_sub_parts() {
        for code in BASIC_PART_OF_SPEECH_CODES {
            assert!(is_basic_part_of_speech(code), "{code} 应允许细分词性");
        }
        for code in [
            "preposition",
            "interjection",
            "particle",
            "NOUN",
            "",
            "noun ",
        ] {
            assert!(!is_basic_part_of_speech(code), "{code:?} 不应允许细分词性");
        }
    }
}
