use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

pub const HEADWORD_NORMALIZATION_VERSION: i16 = 1;
pub const MAX_HEADWORD_CODEPOINTS: usize = 200;

// 录入词条允许的字符：拉丁字母（含预组合变音符）、组合变音符、数字、半角空格与常见连接符。
// 归一化不一定能把变音符合成掉（`q` + U+0301 没有预组合字符），只看 Script=Latin 会漏，
// 故并入 `\p{Mark}`。空白只写半角空格而不是 `\s`：全角空格、不换行空格已被
// `collapse_whitespace` 折叠成它，控制字符更早就被拒了，能走到这里的空白只有这一种。
// 逗号是为 `day in, day out` 这类短语开的——内置词典里有 663 条这样的正经词条。
// 规则与 admin 前端 `headwordValidation.ts` 对齐——任一侧放宽，脏词条就会从另一侧漏进来。
static ALLOWED_HEADWORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\p{Script=Latin}\p{Mark}0-9 '’\-.&/,]+$").expect("headword 字符集正则应可编译")
});
// 纯数字、纯符号能通过字符集检查但都不是词条，需要单独一条兜底。
static LATIN_LETTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\p{Script=Latin}").expect("拉丁字母正则应可编译"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHeadword {
    pub display: String,
    pub key: String,
}

impl NormalizedHeadword {
    /// 解析管理员**录入**的 headword：先归一化再校验字符集，落库的就是校验通过的那份值。
    ///
    /// 内置词典未命中时创建路径依然放行（口子是留给新造词、品牌名、缩写和行业术语的），
    /// 中文、假名、纯数字本不在豁免之列。前端录入规则只挡得住管理员误操作，挡不住直接
    /// 拿 admin token 调 API，字符集必须在这一层兜底。
    ///
    /// 只读路径——搜索词、已入库数据的投影、词典导入——继续用 [`normalize_headword`]：
    /// 它们不是录入，不该因为字符集被拒。
    pub fn parse(value: &str) -> Result<Self, HeadwordNormalizationError> {
        let normalized = normalize_headword(value)?;
        if !ALLOWED_HEADWORD_RE.is_match(&normalized.display) {
            return Err(HeadwordNormalizationError::UnsupportedCharacter);
        }
        if !LATIN_LETTER_RE.is_match(&normalized.display) {
            return Err(HeadwordNormalizationError::MissingLatinLetter);
        }
        Ok(normalized)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HeadwordNormalizationError {
    #[error("headword is empty")]
    Empty,
    #[error("headword is too long")]
    TooLong,
    #[error("headword contains control characters")]
    ControlCharacter,
    #[error("headword contains unsupported characters")]
    UnsupportedCharacter,
    #[error("headword has no Latin letter")]
    MissingLatinLetter,
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
    fn parse_accepts_english_headwords() {
        for value in [
            "table",
            "give up",
            "don't",
            "don\u{2019}t",
            "e-mail",
            "U.S.A.",
            "R&D",
            "and/or",
            "day in, day out",
            "COVID-19",
            "caf\u{e9}",
            "cafe\u{301}",
            "na\u{ef}ve",
            "  table  ",
            "COLOUR",
        ] {
            assert!(
                NormalizedHeadword::parse(value).is_ok(),
                "合法英文词条被拦：{value}"
            );
        }
    }

    #[test]
    fn parse_rejects_non_latin_scripts() {
        for value in ["苹果", "apple苹果", "りんご", "사과", "яблоко", "apple🍎"] {
            assert_eq!(
                NormalizedHeadword::parse(value).unwrap_err(),
                HeadwordNormalizationError::UnsupportedCharacter,
                "非拉丁字符未被拦：{value}"
            );
        }
    }

    #[test]
    fn parse_rejects_values_without_a_latin_letter() {
        // 已知误伤：24/7 是真实存在的英语词条，为拦住 123456 这类垃圾录入一并拦下。
        for value in ["123456", "---", "24/7"] {
            assert_eq!(
                NormalizedHeadword::parse(value).unwrap_err(),
                HeadwordNormalizationError::MissingLatinLetter,
                "不含字母的输入未被拦：{value}"
            );
        }
    }

    #[test]
    fn parse_keeps_the_normalization_contract() {
        // 归一化先于校验：全角字母折叠成 ASCII 后才是入库形态，校验的就是那份值。
        assert_eq!(
            NormalizedHeadword::parse("\u{ff23}\u{ff25}\u{ff2e}\u{ff34}\u{ff25}\u{ff32}").unwrap(),
            normalize_headword("CENTER").unwrap()
        );
        // 全角空格是中文输入法的产物，前端直接拒；后端归一化已把它折叠成半角空格，
        // 落库与手打空格是同一条记录，故按合法放行而不是报字符集错误。
        assert_eq!(
            NormalizedHeadword::parse("give\u{3000}up").unwrap().display,
            "give up"
        );
        // 空、超长、控制字符仍走原有分支，不被字符集错误盖掉。
        assert_eq!(
            NormalizedHeadword::parse("   ").unwrap_err(),
            HeadwordNormalizationError::Empty
        );
        assert_eq!(
            NormalizedHeadword::parse(&"a".repeat(201)).unwrap_err(),
            HeadwordNormalizationError::TooLong
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
