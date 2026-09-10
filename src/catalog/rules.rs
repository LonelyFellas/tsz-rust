//! 释义必填细分词性的词性规则。
//!
//! 这五个词性下的释义必须选中细分词性，其余词性（含管理员自建的）选填。
//! 这是产品固定口径而非可配置状态，所以不落库，由后端按 `code` 派生并通过
//! `sub_pos_required` 下发；前端只读该字段，不得复制这份编码集合。
//!
//! 注意：本规则与「能不能挂细分词性」无关——任意基本词性都可以扩展细分词性。

/// 释义必须选中细分词性的词性编码。
pub const SUB_POS_REQUIRED_PART_CODES: [&str; 5] =
    ["noun", "verb", "pronoun", "adjective", "adverb"];

/// 该词性下的释义是否必须选中细分词性。
pub fn requires_sub_pos(code: &str) -> bool {
    SUB_POS_REQUIRED_PART_CODES.contains(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_five_codes_require_sub_pos() {
        for code in SUB_POS_REQUIRED_PART_CODES {
            assert!(requires_sub_pos(code), "{code} 的释义应必填细分词性");
        }
        for code in [
            "preposition",
            "interjection",
            "particle",
            "NOUN",
            "",
            "noun ",
        ] {
            assert!(!requires_sub_pos(code), "{code:?} 的释义不应必填细分词性");
        }
    }
}
