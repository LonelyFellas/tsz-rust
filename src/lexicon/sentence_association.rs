//! 例句自动关联的口径：切词、停用词、参与关联的词性，以及正文指纹。
//!
//! 这一层是纯函数，不碰数据库——解析的其余部分（查候选词形、判歧义、落库）在
//! `service/sentence_association.rs`。口径写在这里是为了让它可以被单测逐条钉死。

use sha2::{Digest, Sha256};

use crate::lexicon::{
    dto::{DraftMeaningsStepContent, SentenceAssociationsStateV2},
    normalization::normalize_headword,
};

/// 口径版本。切词规则、停用词表、词性闸、歧义策略任一变更都要 +1：
/// 已解析过的例句会因为版本对不上，在各自下次发布时自然重算，不需要数据迁移。
pub(crate) const RESOLVER_VERSION: i16 = 1;

const V2_ASSOCIATION_FORM_SOURCE_KINDS: &[&str] = &["form"];
const V3_ASSOCIATION_FORM_SOURCE_KINDS: &[&str] = &["form", "form_variant"];

/// rollback 时只消费 V2 publication；调用方确认所需 V3 capability 后才纳入
/// `form_variant`，避免关闭开关后旧 V3 publication 仍被后台 resolver 静默读取。
pub(crate) fn association_form_source_kinds(allow_v3: bool) -> &'static [&'static str] {
    if allow_v3 {
        V3_ASSOCIATION_FORM_SOURCE_KINDS
    } else {
        V2_ASSOCIATION_FORM_SOURCE_KINDS
    }
}

/// 单个词面能有多长。与 `lexicon.sentence_associations.surface` 的库层约束一致——
/// 自动切词和人工补关联都得先在这一层挡住，否则会以 CHECK 违例的形式变成 500。
pub(crate) const MAX_ASSOCIATION_SURFACE_CODEPOINTS: usize = 200;

/// 纯语法词。命中即跳过，早于任何数据库查询。
///
/// 为什么光靠 §词性闸不够：`is` 在词库里是 `be` 的第三人称单数形式，词性是 verb，
/// 不列在这里就会被关联上。反过来 `the` / `a` / `on` / `it` 由词性闸拦下，不必进表。
///
/// 必须保持字典序——`is_stopword` 走二分查找，`stopwords_are_sorted` 守着这条。
const STOPWORDS: &[&str] = &[
    "am", "are", "be", "been", "being", "can", "could", "did", "do", "does", "doing", "done",
    "had", "has", "have", "having", "is", "may", "might", "must", "not", "ought", "shall",
    "should", "was", "were", "will", "would",
];

/// 正文里切出的一个候选词。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SentenceToken {
    /// `RichText.text` 的 Unicode 码点下标，左闭右开。
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// 原句词面，等于 `text[start..end]`（按码点切）。
    pub(crate) surface: String,
    /// `normalize_headword` 的归一化键，用来跟 `surface_sources.normalized_surface` 对齐。
    pub(crate) normalized: String,
}

/// 把例句正文切成候选词。
///
/// 词内允许撇号与真连字符（`don't`、`well-known` 各算一个词）；破折号（U+2013/U+2014）
/// 是标点不是连字符，不并词。组合附加符号跟着字母走，否则 `cafe` + U+0301 会被拦腰切开。
pub(crate) fn tokenize(text: &str) -> Vec<SentenceToken> {
    let mut tokens = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    for (index, character) in text.chars().enumerate() {
        if is_token_char(character) {
            match current.as_mut() {
                Some((_, end)) => *end = index + 1,
                None => current = Some((index, index + 1)),
            }
        } else if let Some((start, end)) = current.take() {
            push_token(&mut tokens, text, start, end);
        }
    }
    if let Some((start, end)) = current {
        push_token(&mut tokens, text, start, end);
    }
    tokens
}

/// 人工 Pending 短语只允许多个单词以空白连续连接；逗号、斜杠等标点不能被框进
/// 一个短语 range，避免把两个相邻成分误建成同一词条。
pub(crate) fn is_contiguous_phrase_surface(text: &str) -> bool {
    let tokens = tokenize(text);
    if tokens.len() < 2
        || tokens[0].start != 0
        || tokens
            .last()
            .is_none_or(|token| token.end != text.chars().count())
    {
        return false;
    }
    tokens.windows(2).all(|pair| {
        codepoint_slice(text, pair[0].end, pair[1].start)
            .is_some_and(|gap| !gap.is_empty() && gap.chars().all(char::is_whitespace))
    })
}

/// 词面能不能落进 `lexicon.sentence_associations.surface`。
///
/// 库层的 `surface = btrim(surface)` 与 200 码点上限必须在服务层先挡一道：
/// 人工补关联的区间是客户端给的，框选时多带一个尾随空格就会一路走到 INSERT，
/// 以 CHECK 违例的形式变成 500 而不是 422。`str::trim` 比 `btrim` 更严，
/// 过得了这里就一定过得了库层。
pub(crate) fn is_storable_surface(surface: &str) -> bool {
    surface.trim() == surface
        && !surface.is_empty()
        && surface.chars().count() <= MAX_ASSOCIATION_SURFACE_CODEPOINTS
}

pub(crate) fn is_stopword(normalized: &str) -> bool {
    STOPWORDS.binary_search(&normalized).is_ok()
}

/// 参与自动关联的词性：实词。
///
/// 词形类型能力对所有 POS 开放后，这里仍保留原有实词范围；自定义 POS fail closed 不关联。
pub(crate) fn associable_pos(part_of_speech: &str) -> bool {
    matches!(part_of_speech, "noun" | "verb" | "adjective" | "adverb")
}

/// 清空只读投影。两处都要用：草稿保存时客户端可能把读到的关联原样回传，
/// 发布快照则是历史存档，不该把一份按 entry 单独维护的数据跟着冻进去。
pub(crate) fn clear_sentence_associations(meanings: &mut DraftMeaningsStepContent) {
    for pos in &mut meanings.pos {
        for sense in &mut pos.senses {
            for sentence in &mut sense.sentences {
                sentence.associations = Vec::new();
                sentence.associations_state = SentenceAssociationsStateV2::Unresolved;
            }
        }
    }
}

/// 正文指纹。正文没变就不重算关联，管理员的事后修正因此能活过下一次发布。
pub(crate) fn text_hash(text: &str) -> Vec<u8> {
    Sha256::digest(text.as_bytes()).to_vec()
}

/// 按**码点**下标取子串；越界或空区间返回 `None`。
pub(crate) fn codepoint_slice(text: &str, start: usize, end: usize) -> Option<&str> {
    if end <= start || end > text.chars().count() {
        return None;
    }
    let byte_start = text.char_indices().nth(start).map(|(offset, _)| offset)?;
    let byte_end = text
        .char_indices()
        .nth(end)
        .map_or(text.len(), |(offset, _)| offset);
    Some(&text[byte_start..byte_end])
}

fn is_token_char(character: char) -> bool {
    character.is_alphanumeric() || is_connector(character) || is_combining_mark(character)
}

/// 词内连接符。ASCII 撇号与两种排版撇号、ASCII 连字符与两种真连字符——
/// 都是 `normalize_headword` 会折叠成 `'` / `-` 的那几个。
fn is_connector(character: char) -> bool {
    matches!(
        character,
        '\'' | '\u{2018}' | '\u{2019}' | '\u{02bc}' | '-' | '\u{2010}' | '\u{2011}'
    )
}

fn is_combining_mark(character: char) -> bool {
    matches!(character, '\u{0300}'..='\u{036f}')
}

fn push_token(tokens: &mut Vec<SentenceToken>, text: &str, start: usize, end: usize) {
    // 词首词尾的连接符是标点，不是词的一部分：`'picture'` 切出来的是 picture。
    let mut start = start;
    let mut end = end;
    let characters = text.chars().collect::<Vec<_>>();
    while start < end && is_connector(characters[start]) {
        start += 1;
    }
    while end > start && is_connector(characters[end - 1]) {
        end -= 1;
    }
    if start >= end {
        return;
    }
    let Some(surface) = codepoint_slice(text, start, end) else {
        return;
    };
    if !is_storable_surface(surface) {
        return;
    }
    // 只读口径：不做字符集校验，搜索词和已入库投影都走这条。
    let Ok(normalized) = normalize_headword(surface) else {
        return;
    };
    tokens.push(SentenceToken {
        start,
        end,
        surface: surface.to_owned(),
        normalized: normalized.key,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopwords_are_sorted_and_cover_the_auxiliaries_that_pass_the_pos_gate() {
        let mut sorted = STOPWORDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(STOPWORDS, sorted.as_slice(), "停用词表必须保持字典序");

        // 这些都是动词形态，词性闸放行，只能靠停用词表拦。
        for word in ["is", "are", "was", "have", "did", "would", "must"] {
            assert!(is_stopword(word), "{word} 应在停用词表内");
        }
        // 这些由词性闸拦下，不该出现在表里——表越大越容易误伤实词。
        for word in ["the", "a", "on", "it", "and"] {
            assert!(!is_stopword(word), "{word} 该由词性闸拦下，不进停用词表");
        }
        for word in ["picture", "wall", "center", "run"] {
            assert!(!is_stopword(word));
        }
    }

    #[test]
    fn only_parts_of_speech_with_inflections_are_associable() {
        for part in ["noun", "verb", "adjective", "adverb"] {
            assert!(associable_pos(part));
        }
        for part in [
            "article",
            "determiner",
            "preposition",
            "pronoun",
            "conjunction",
            "numeral",
            "interjection",
            "custom_part",
        ] {
            assert!(!associable_pos(part), "{part} 不该参与自动关联");
        }
    }

    #[test]
    fn tokenizer_reports_codepoint_ranges_that_slice_back_to_the_surface() {
        let text = "Center the picture on the wall.";
        let tokens = tokenize(text);

        let surfaces = tokens
            .iter()
            .map(|token| token.surface.as_str())
            .collect::<Vec<_>>();
        assert_eq!(surfaces, ["Center", "the", "picture", "on", "the", "wall"]);
        for token in &tokens {
            assert_eq!(
                codepoint_slice(text, token.start, token.end),
                Some(token.surface.as_str())
            );
        }
        assert_eq!(tokens[2].start, 11);
        assert_eq!(tokens[2].end, 18);
        assert_eq!(tokens[0].normalized, "center");
    }

    #[test]
    fn the_same_spelling_twice_yields_two_distinct_positions() {
        let tokens = tokenize("A wall behind the wall.");
        let walls = tokens
            .iter()
            .filter(|token| token.normalized == "wall")
            .collect::<Vec<_>>();

        assert_eq!(walls.len(), 2);
        assert_ne!(walls[0].start, walls[1].start);
    }

    #[test]
    fn word_internal_connectors_stay_but_punctuation_does_not() {
        let tokens = tokenize("Don't call a 'well-known' one—ever.");
        let surfaces = tokens
            .iter()
            .map(|token| token.surface.as_str())
            .collect::<Vec<_>>();

        // 撇号与真连字符留在词里；引号被剥掉；破折号断词。
        assert_eq!(
            surfaces,
            ["Don't", "call", "a", "well-known", "one", "ever"]
        );
        assert_eq!(tokens[0].normalized, "don't");
    }

    #[test]
    fn combining_marks_do_not_split_a_word() {
        let text = "A cafe\u{301} nearby.";
        let tokens = tokenize(text);

        assert_eq!(tokens[1].surface, "cafe\u{301}");
        assert_eq!(tokens[1].normalized, "café");
    }

    #[test]
    fn codepoint_slice_rejects_out_of_range_and_empty_spans() {
        let text = "café wall";
        assert_eq!(codepoint_slice(text, 0, 4), Some("café"));
        assert_eq!(codepoint_slice(text, 5, 9), Some("wall"));
        assert_eq!(codepoint_slice(text, 5, 10), None);
        assert_eq!(codepoint_slice(text, 5, 5), None);
    }

    #[test]
    fn storable_surface_matches_the_column_constraint() {
        assert!(is_storable_surface("wall"));
        assert!(is_storable_surface("well-known"));
        // 库层是 surface = btrim(surface)：首尾空白一律不收。
        assert!(!is_storable_surface("wall "));
        assert!(!is_storable_surface(" wall"));
        assert!(!is_storable_surface(""));
        assert!(is_storable_surface(
            &"a".repeat(MAX_ASSOCIATION_SURFACE_CODEPOINTS)
        ));
        assert!(!is_storable_surface(
            &"a".repeat(MAX_ASSOCIATION_SURFACE_CODEPOINTS + 1)
        ));
    }

    #[test]
    fn text_hash_tracks_the_exact_body() {
        assert_eq!(text_hash("a wall"), text_hash("a wall"));
        assert_ne!(text_hash("a wall"), text_hash("a Wall"));
    }
}
