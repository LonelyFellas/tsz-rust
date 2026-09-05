use std::collections::{HashMap, HashSet};

use regex::Regex;

use crate::lexicon::{
    dto::{DialectVariantSuggestionItemV2, RichText, RichTextAnnotation, RichTextV1, RichTextV2},
    normalization::normalize_headword,
};

pub(crate) const PROVIDER_KIND: &str = "dictionary_region_rules";
pub(crate) const PROVIDER_VERSION: &str = "1";

pub(crate) trait DialectSuggestionProvider {
    fn kind(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn suggest(
        &self,
        item: &DialectVariantSuggestionItemV2,
        replacements: &HashMap<String, String>,
    ) -> Option<DialectVariantSuggestionItemV2>;
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DictionaryRegionRulesProvider;

impl DialectSuggestionProvider for DictionaryRegionRulesProvider {
    fn kind(&self) -> &'static str {
        PROVIDER_KIND
    }

    fn version(&self) -> &'static str {
        PROVIDER_VERSION
    }

    fn suggest(
        &self,
        item: &DialectVariantSuggestionItemV2,
        replacements: &HashMap<String, String>,
    ) -> Option<DialectVariantSuggestionItemV2> {
        match item {
            DialectVariantSuggestionItemV2::Form {
                client_id,
                field_kind,
                value,
            } => {
                let converted = convert_plain_text(value, replacements, &HashSet::new(), &[]);
                (converted.text != *value).then(|| DialectVariantSuggestionItemV2::Form {
                    client_id: client_id.clone(),
                    field_kind: *field_kind,
                    value: converted.text,
                })
            }
            DialectVariantSuggestionItemV2::RichText {
                client_id,
                field_kind,
                value,
            } => {
                let converted = convert_rich_text(value, replacements);
                (converted.text() != value.text()).then(|| {
                    DialectVariantSuggestionItemV2::RichText {
                        client_id: client_id.clone(),
                        field_kind: *field_kind,
                        value: converted,
                    }
                })
            }
        }
    }
}

pub(crate) fn evidence_keys(items: &[DialectVariantSuggestionItemV2]) -> Vec<String> {
    let mut keys = HashSet::new();
    for item in items {
        let value = match item {
            DialectVariantSuggestionItemV2::Form { value, .. } => value.as_str(),
            DialectVariantSuggestionItemV2::RichText { value, .. } => value.text(),
        };
        if let Ok(normalized) = normalize_headword(value) {
            keys.insert(normalized.key);
        }
        for token in token_ranges(value) {
            if let Ok(normalized) = normalize_headword(&token.value) {
                keys.insert(normalized.key);
            }
        }
    }
    let mut keys = keys.into_iter().collect::<Vec<_>>();
    keys.sort();
    keys
}

#[derive(Debug)]
struct ConvertedText {
    text: String,
    boundary_map: Vec<usize>,
}

#[derive(Debug)]
struct TokenRange {
    start: usize,
    end: usize,
    value: String,
}

#[derive(Debug)]
struct Replacement {
    start: usize,
    end: usize,
    value: String,
}

fn token_ranges(value: &str) -> Vec<TokenRange> {
    let regex =
        Regex::new(r"[A-Za-z]+(?:['’\-][A-Za-z]+)*").expect("the English token pattern is valid");
    regex
        .find_iter(value)
        .map(|matched| TokenRange {
            start: value[..matched.start()].chars().count(),
            end: value[..matched.end()].chars().count(),
            value: matched.as_str().to_owned(),
        })
        .collect()
}

fn convert_rich_text(value: &RichText, replacements: &HashMap<String, String>) -> RichText {
    let mut output = value.clone();
    match &mut output {
        RichText::V1(document) => {
            let mut protected_boundaries = HashSet::new();
            for span in &document.spans {
                protected_boundaries.insert(span.start);
                protected_boundaries.insert(span.end);
            }
            protected_boundaries.extend(document.liaisons.iter().copied());
            let converted =
                convert_plain_text(&document.text, replacements, &protected_boundaries, &[]);
            document.text = converted.text;
            remap_v1(document, &converted.boundary_map);
        }
        RichText::V2(document) => {
            let mut protected_boundaries = HashSet::new();
            let mut protected_ranges = Vec::new();
            for annotation in &document.annotations {
                match annotation {
                    RichTextAnnotation::Liaison {
                        start,
                        end,
                        start_len,
                        end_len,
                    } => {
                        protected_boundaries.insert(*start);
                        protected_boundaries.insert(*end);
                        // 锚点的内侧边界只在真的存了宽度时才保护。宽度为 1 的存量内容
                        // 多护两个边界，只会白白挡掉替换——"colour" 上挂一条连读就再也
                        // 换不成 "color"，而那两个边界本来就没有信息可保。
                        if *start_len > 1 {
                            protected_boundaries.insert(*start + *start_len);
                        }
                        if *end_len > 1 {
                            protected_boundaries.insert(*end - *end_len);
                        }
                    }
                    RichTextAnnotation::Emphasis { start, end, .. }
                    | RichTextAnnotation::Highlight { start, end, .. } => {
                        protected_boundaries.insert(*start);
                        protected_boundaries.insert(*end);
                    }
                    RichTextAnnotation::Phoneme { start, end, .. } => {
                        protected_boundaries.insert(*start);
                        protected_boundaries.insert(*end);
                        protected_ranges.push((*start, *end));
                    }
                    RichTextAnnotation::Pause { at, .. } => {
                        protected_boundaries.insert(*at);
                    }
                }
            }
            let converted = convert_plain_text(
                &document.text,
                replacements,
                &protected_boundaries,
                &protected_ranges,
            );
            document.text = converted.text;
            remap_v2(document, &converted.boundary_map);
        }
    }
    output
}

fn convert_plain_text(
    value: &str,
    replacements: &HashMap<String, String>,
    protected_boundaries: &HashSet<usize>,
    protected_ranges: &[(usize, usize)],
) -> ConvertedText {
    let old = value.chars().collect::<Vec<_>>();
    let mut operations = Vec::new();

    if let Ok(normalized) = normalize_headword(value)
        && let Some(target) = replacements.get(&normalized.key)
        && protected_boundaries
            .iter()
            .all(|position| *position == 0 || *position == old.len())
        && protected_ranges.is_empty()
    {
        operations.push(Replacement {
            start: 0,
            end: old.len(),
            value: preserve_case(value, target),
        });
    } else {
        for token in token_ranges(value) {
            let Ok(normalized) = normalize_headword(&token.value) else {
                continue;
            };
            let Some(target) = replacements.get(&normalized.key) else {
                continue;
            };
            let has_internal_boundary = protected_boundaries
                .iter()
                .any(|position| token.start < *position && *position < token.end);
            let overlaps_protected_range = protected_ranges
                .iter()
                .any(|(start, end)| token.start < *end && *start < token.end);
            if has_internal_boundary || overlaps_protected_range {
                continue;
            }
            operations.push(Replacement {
                start: token.start,
                end: token.end,
                value: preserve_case(&token.value, target),
            });
        }
    }

    let mut text = String::new();
    let mut boundary_map = vec![0; old.len() + 1];
    let mut old_cursor = 0;
    let mut new_cursor = 0;
    for operation in operations {
        for (offset, boundary) in boundary_map[old_cursor..=operation.start]
            .iter_mut()
            .enumerate()
        {
            *boundary = new_cursor + offset;
        }
        text.extend(old[old_cursor..operation.start].iter());
        new_cursor += operation.start - old_cursor;

        let replacement_len = operation.value.chars().count();
        let old_len = operation.end - operation.start;
        for offset in 0..=old_len {
            boundary_map[operation.start + offset] =
                new_cursor + (offset * replacement_len / old_len.max(1));
        }
        text.push_str(&operation.value);
        new_cursor += replacement_len;
        old_cursor = operation.end;
    }
    for (offset, boundary) in boundary_map[old_cursor..].iter_mut().enumerate() {
        *boundary = new_cursor + offset;
    }
    text.extend(old[old_cursor..].iter());
    ConvertedText { text, boundary_map }
}

fn preserve_case(source: &str, target: &str) -> String {
    if source.chars().any(char::is_alphabetic)
        && source
            .chars()
            .filter(|character| character.is_alphabetic())
            .all(|character| character.is_uppercase())
    {
        return target.to_uppercase();
    }
    let mut source_chars = source.chars().filter(|character| character.is_alphabetic());
    if source_chars.next().is_some_and(char::is_uppercase) && source_chars.all(char::is_lowercase) {
        let mut target_chars = target.chars();
        if let Some(first) = target_chars.next() {
            return first.to_uppercase().chain(target_chars).collect();
        }
    }
    target.to_owned()
}

fn remap_v1(document: &mut RichTextV1, map: &[usize]) {
    for span in &mut document.spans {
        span.start = map[span.start];
        span.end = map[span.end];
    }
    for liaison in &mut document.liaisons {
        *liaison = map[*liaison];
    }
}

fn remap_v2(document: &mut RichTextV2, map: &[usize]) {
    for annotation in &mut document.annotations {
        match annotation {
            RichTextAnnotation::Liaison {
                start,
                end,
                start_len,
                end_len,
            } => {
                let anchor_start_end = map[*start + *start_len];
                let anchor_end_start = map[*end - *end_len];
                let next_start = map[*start];
                let next_end = map[*end];
                let span = next_end.saturating_sub(next_start).max(1);
                *start = next_start;
                *end = next_end;
                // 宽度为 1 的锚点没有信息可搬，原样留 1——比例映射会把单个码点撑成
                // 两个（color→colour 时末字母 "r" 会变成 "ur"）。只有真存了宽度的
                // 锚点才跟着重算，并夹在 [1, span] 内，免得建议接口产出 validate_v2
                // 自己会以 invalid_liaison_anchor 拒掉的标注。
                if *start_len > 1 {
                    *start_len = anchor_start_end.saturating_sub(next_start).clamp(1, span);
                }
                if *end_len > 1 {
                    *end_len = next_end.saturating_sub(anchor_end_start).clamp(1, span);
                }
            }
            RichTextAnnotation::Emphasis { start, end, .. }
            | RichTextAnnotation::Phoneme { start, end, .. }
            | RichTextAnnotation::Highlight { start, end, .. } => {
                *start = map[*start];
                *end = map[*end];
            }
            RichTextAnnotation::Pause { at, .. } => *at = map[*at],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::dto::{RichTextEmphasisLevel, RichTextPhonemeAlphabet, RichTextV2};

    fn replacements() -> HashMap<String, String> {
        HashMap::from([
            ("color".to_owned(), "colour".to_owned()),
            ("center".to_owned(), "centre".to_owned()),
        ])
    }

    #[test]
    fn evidence_keys_and_case_preserving_replacements_are_deterministic() {
        let items = vec![DialectVariantSuggestionItemV2::Form {
            client_id: "one".to_owned(),
            field_kind: crate::lexicon::dto::DialectSuggestionFieldKind::Form,
            value: "Color CENTER".to_owned(),
        }];
        assert_eq!(
            evidence_keys(&items),
            vec!["center", "color", "color center"]
        );
        let suggestion = DictionaryRegionRulesProvider
            .suggest(&items[0], &replacements())
            .unwrap();
        let DialectVariantSuggestionItemV2::Form { value, .. } = suggestion else {
            panic!("expected form suggestion");
        };
        assert_eq!(value, "Colour CENTRE");
    }

    #[test]
    fn rich_text_offsets_follow_replacement_length() {
        let item = DialectVariantSuggestionItemV2::RichText {
            client_id: "rich".to_owned(),
            field_kind: crate::lexicon::dto::DialectSuggestionFieldKind::Definition,
            value: RichText::V2(RichTextV2 {
                version: 2,
                text: "Color is central".to_owned(),
                annotations: vec![RichTextAnnotation::Emphasis {
                    start: 0,
                    end: 5,
                    level: RichTextEmphasisLevel::Strong,
                }],
            }),
        };
        let suggestion = DictionaryRegionRulesProvider
            .suggest(&item, &replacements())
            .unwrap();
        let DialectVariantSuggestionItemV2::RichText {
            value: RichText::V2(value),
            ..
        } = suggestion
        else {
            panic!("expected V2 rich text suggestion");
        };
        assert_eq!(value.text, "Colour is central");
        assert!(matches!(
            value.annotations[0],
            RichTextAnnotation::Emphasis {
                start: 0,
                end: 6,
                ..
            }
        ));
    }

    #[test]
    fn liaison_widths_survive_replacement_without_blocking_it() {
        // 宽度全为默认的存量内容：不该因为挂了一条连读就换不成方言拼写。
        let legacy = RichText::V2(RichTextV2 {
            version: 2,
            text: "color".to_owned(),
            annotations: vec![RichTextAnnotation::Liaison {
                start: 0,
                end: 5,
                start_len: 1,
                end_len: 1,
            }],
        });
        let converted = convert_rich_text(&legacy, &replacements());
        assert_eq!(converted.text(), "colour");
        let RichText::V2(converted) = converted else {
            panic!("expected V2")
        };
        assert!(matches!(
            converted.annotations[0],
            RichTextAnnotation::Liaison {
                start: 0,
                end: 6,
                start_len: 1,
                end_len: 1,
            }
        ));

        // 存了宽度的内容：锚点内侧边界受保护，落在锚点里的词不再被替换。
        let anchored = RichText::V2(RichTextV2 {
            version: 2,
            text: "color center".to_owned(),
            annotations: vec![RichTextAnnotation::Liaison {
                start: 0,
                end: 12,
                start_len: 2,
                end_len: 2,
            }],
        });
        let converted = convert_rich_text(&anchored, &replacements());
        assert_eq!(converted.text(), "color center");
        let RichText::V2(converted) = converted else {
            panic!("expected V2")
        };
        assert!(matches!(
            converted.annotations[0],
            RichTextAnnotation::Liaison {
                start_len: 2,
                end_len: 2,
                ..
            }
        ));

        // 无论怎么映射，产出的锚点都必须是 validate_v2 认的形状。
        for value in [legacy, anchored] {
            let converted = convert_rich_text(&value, &replacements());
            assert!(
                crate::lexicon::rich_text::is_valid(&converted),
                "方言建议不能产出自己校验器拒绝的标注：{converted:?}"
            );
        }
    }

    #[test]
    fn phoneme_ranges_protect_their_token() {
        let value = RichText::V2(RichTextV2 {
            version: 2,
            text: "color center".to_owned(),
            annotations: vec![RichTextAnnotation::Phoneme {
                start: 0,
                end: 5,
                alphabet: RichTextPhonemeAlphabet::Ipa,
                phoneme: "kʌlə".to_owned(),
            }],
        });
        let converted = convert_rich_text(&value, &replacements());
        assert_eq!(converted.text(), "color centre");
    }
}
