use super::*;
use crate::lexicon::dto::{RichTextEmphasisLevel, RichTextHighlightColor, RichTextPhonemeAlphabet};

#[test]
fn canonicalizes_ranges_and_last_pause_deterministically() {
    let mut value = RichText::V2(RichTextV2 {
        version: 2,
        text: "test".to_owned(),
        annotations: vec![
            RichTextAnnotation::Emphasis {
                start: 2,
                end: 4,
                level: RichTextEmphasisLevel::Strong,
            },
            RichTextAnnotation::Highlight {
                start: 1,
                end: 3,
                color: RichTextHighlightColor::Green,
            },
            RichTextAnnotation::Emphasis {
                start: 0,
                end: 2,
                level: RichTextEmphasisLevel::Strong,
            },
            RichTextAnnotation::Pause {
                at: 4,
                duration_ms: 200,
            },
            RichTextAnnotation::Pause {
                at: 4,
                duration_ms: 800,
            },
        ],
    });
    canonicalize(&mut value).unwrap();
    let RichText::V2(value) = value else {
        panic!("expected V2")
    };
    assert_eq!(value.annotations.len(), 3);
    assert!(matches!(
        value.annotations[0],
        RichTextAnnotation::Emphasis {
            start: 0,
            end: 4,
            ..
        }
    ));
    assert!(matches!(
        value.annotations[2],
        RichTextAnnotation::Pause {
            at: 4,
            duration_ms: 800
        }
    ));
}

#[test]
fn rejects_invalid_speech_ranges_and_pause() {
    let mut value = RichText::V2(RichTextV2 {
        version: 2,
        text: "test".to_owned(),
        annotations: vec![
            RichTextAnnotation::Phoneme {
                start: 0,
                end: 3,
                alphabet: RichTextPhonemeAlphabet::Ipa,
                phoneme: "a".to_owned(),
            },
            RichTextAnnotation::Emphasis {
                start: 1,
                end: 4,
                level: RichTextEmphasisLevel::Strong,
            },
            RichTextAnnotation::Pause {
                at: 2,
                duration_ms: 300,
            },
        ],
    });
    let codes = canonicalize(&mut value)
        .unwrap_err()
        .into_iter()
        .map(|issue| issue.code)
        .collect::<Vec<_>>();
    assert!(codes.contains(&"crossing_speech_marks"));
    assert!(codes.contains(&"pause_inside_phoneme"));
}

#[test]
fn canonicalization_is_idempotent_and_uses_frontend_type_order() {
    let mut value = RichText::V2(RichTextV2 {
        version: 2,
        text: "abcdef".to_owned(),
        annotations: vec![
            RichTextAnnotation::Phoneme {
                start: 0,
                end: 1,
                alphabet: RichTextPhonemeAlphabet::Ipa,
                phoneme: "a".to_owned(),
            },
            RichTextAnnotation::Emphasis {
                start: 0,
                end: 1,
                level: RichTextEmphasisLevel::Strong,
            },
            RichTextAnnotation::Liaison { start: 0, end: 1 },
            RichTextAnnotation::Highlight {
                start: 0,
                end: 1,
                color: RichTextHighlightColor::Blue,
            },
            RichTextAnnotation::Pause {
                at: 0,
                duration_ms: 1,
            },
        ],
    });
    canonicalize(&mut value).unwrap();
    let once = serde_json::to_value(&value).unwrap();
    canonicalize(&mut value).unwrap();
    assert_eq!(serde_json::to_value(&value).unwrap(), once);
    let RichText::V2(value) = value else {
        panic!("expected V2")
    };
    assert!(matches!(
        value.annotations[0],
        RichTextAnnotation::Pause { .. }
    ));
    assert!(matches!(
        value.annotations[1],
        RichTextAnnotation::Highlight { .. }
    ));
    assert!(matches!(
        value.annotations[2],
        RichTextAnnotation::Liaison { .. }
    ));
    assert!(matches!(
        value.annotations[3],
        RichTextAnnotation::Emphasis { .. }
    ));
    assert!(matches!(
        value.annotations[4],
        RichTextAnnotation::Phoneme { .. }
    ));
}

#[test]
fn validates_original_annotation_count_before_merging() {
    let mut value = RichText::V2(RichTextV2 {
        version: 2,
        text: "ab".to_owned(),
        annotations: (0..=MAX_RICH_TEXT_ANNOTATIONS)
            .map(|_| RichTextAnnotation::Emphasis {
                start: 0,
                end: 1,
                level: RichTextEmphasisLevel::Strong,
            })
            .collect(),
    });
    assert!(
        canonicalize(&mut value)
            .unwrap_err()
            .iter()
            .any(|issue| issue.code == "too_many_annotations")
    );
}

#[test]
fn v1_liaison_uses_checked_codepoint_arithmetic() {
    let mut value = RichText::V1(RichTextV1 {
        version: 1,
        text: "ok".to_owned(),
        spans: Vec::new(),
        liaisons: vec![usize::MAX],
    });
    assert_eq!(
        canonicalize(&mut value).unwrap_err()[0].code,
        "invalid_range"
    );
}

#[test]
fn v1_missing_or_null_collections_are_read_as_empty() {
    for json in [
        r#"{"version":1,"text":"legacy"}"#,
        r#"{"version":1,"text":"legacy","spans":null,"liaisons":null}"#,
    ] {
        let value: RichText = serde_json::from_str(json).unwrap();
        let RichText::V1(value) = value else {
            panic!("expected V1")
        };
        assert!(value.spans.is_empty());
        assert!(value.liaisons.is_empty());
    }
}

#[test]
fn rejects_nul_in_all_persisted_rich_text_strings() {
    for mut value in [
        RichText::V1(RichTextV1 {
            version: 1,
            text: "legacy\0text".to_owned(),
            spans: Vec::new(),
            liaisons: Vec::new(),
        }),
        RichText::V2(RichTextV2 {
            version: 2,
            text: "modern\0text".to_owned(),
            annotations: Vec::new(),
        }),
    ] {
        assert!(
            canonicalize(&mut value)
                .unwrap_err()
                .iter()
                .any(|issue| issue.code == "nul_character_not_allowed")
        );
    }

    let mut phoneme = RichText::V2(RichTextV2 {
        version: 2,
        text: "word".to_owned(),
        annotations: vec![RichTextAnnotation::Phoneme {
            start: 0,
            end: 4,
            alphabet: RichTextPhonemeAlphabet::Ipa,
            phoneme: "w\0d".to_owned(),
        }],
    });
    assert!(
        canonicalize(&mut phoneme)
            .unwrap_err()
            .iter()
            .any(|issue| issue.code == "nul_character_not_allowed")
    );
}
