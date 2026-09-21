use super::*;
use serde_json::{Value, json};
use uuid::Uuid;

fn text() -> Value {
    json!({"version": 2, "text": "test", "annotations": [
        {"type": "emphasis", "start": 2, "end": 4, "level": "strong"},
        {"type": "emphasis", "start": 0, "end": 2, "level": "strong"}
    ]})
}

fn complete_content() -> DraftMeaningsStepContentV3 {
    let profile = json!({"voices": [{"voice_id": "sonia", "enabled": true, "rate_percent": 10}]});
    let variant = json!({
        "id": Uuid::now_v7(), "value": text(), "origin": "manual",
        "voice_profile": profile,
        "text_links": [{
            "id": Uuid::now_v7(),
            "source_segments": [{"start": 0, "end": 4, "surface": "test"}],
            "target_word_id": Uuid::now_v7(), "target_pos_id": Uuid::now_v7(),
            "target_base_form_id": Uuid::now_v7(), "target_form_id": Uuid::now_v7(),
            "target_variant_id": Uuid::now_v7(), "target_sense_id": Uuid::now_v7()
        }]
    });
    let mut us_variant = variant.clone();
    us_variant["id"] = json!(Uuid::now_v7());
    serde_json::from_value(json!({
        "sense_groups": [{
            "id": Uuid::now_v7(), "name_zh": "测试", "name_en": "test",
            "name_en_rich": text(), "voice_profile": profile
        }],
        "pos": [{
            "pos_id": Uuid::now_v7(),
            "grammar_structures": [{
                "id": Uuid::now_v7(), "variants": [{
                    "id": Uuid::now_v7(), "dialect": "common", "content": text(),
                    "voice_profile": profile,
                    "audio_assets": [{
                        "id": Uuid::now_v7(), "locale": "en-GB", "gender": "female",
                        "content_type": "audio/mpeg", "size_bytes": 128,
                        "duration_ms": 1000, "original_name": "test.mp3",
                        "created_at": "2026-09-01T00:00:00Z"
                    }]
                }]
            }],
            "senses": [{
                "id": Uuid::now_v7(), "sub_pos": "N-COUNT", "level": "A1",
                "depends_on_context": false,
                "form_group_ids": [Uuid::now_v7(), Uuid::now_v7()],
                "definitions": [{
                    "definition_mode": "zh_definition", "id": Uuid::now_v7(),
                    "content_id": Uuid::now_v7(), "level": "A1", "content": text()
                }, {
                    "definition_mode": "en_definition", "id": Uuid::now_v7(), "level": "A1",
                    "content": {"mode": "unified", "common": variant}
                }],
                "sentences": [{
                    "id": Uuid::now_v7(), "level": "A1",
                    "en_text": {"mode": "distinguish", "source_dialect": "uk",
                        "uk": {"state": "ready", "variant": variant},
                        "us": {"state": "ready", "variant": us_variant}},
                    "zh_text_id": Uuid::now_v7(), "zh_text": text(),
                    "zh_translations": [
                        {"id": Uuid::now_v7(), "band": "word_for_word", "language": "zh", "content": text()},
                        {"id": Uuid::now_v7(), "band": "balanced_fluency", "language": "zh", "content": text()},
                        {"id": Uuid::now_v7(), "band": "adapted_creation", "language": "zh", "content": text()}
                    ],
                    "links": [], "associations": [], "associations_state": "unresolved"
                }],
                "relations": [{"id": Uuid::now_v7(), "relation": "synonym", "score": "50", "pending_target_headword": "trial"}],
                "component_usages": [{"id": Uuid::now_v7(), "state": "unresolved", "literal": "test"}]
            }]
        }]
    })).unwrap()
}

#[test]
fn complete_meanings_are_normalized_in_place_without_losing_extensions() {
    let mut content = complete_content();
    let mut expected = serde_json::to_value(&content).unwrap();
    let paths = [
        "/sense_groups/0/name_en_rich",
        "/pos/0/grammar_structures/0/variants/0/content",
        "/pos/0/senses/0/definitions/0/content",
        "/pos/0/senses/0/definitions/1/content/common/value",
        "/pos/0/senses/0/sentences/0/en_text/uk/variant/value",
        "/pos/0/senses/0/sentences/0/en_text/us/variant/value",
        "/pos/0/senses/0/sentences/0/zh_text",
        "/pos/0/senses/0/sentences/0/zh_translations/0/content",
        "/pos/0/senses/0/sentences/0/zh_translations/1/content",
        "/pos/0/senses/0/sentences/0/zh_translations/2/content",
    ];
    for path in paths {
        expected.pointer_mut(path).unwrap()["annotations"] = json!([
            {"type": "emphasis", "start": 0, "end": 4, "level": "strong"}
        ]);
    }
    assert!(canonicalize_meanings(&mut content));
    // 全文相等，覆盖语音、音频、链接、译文、词义组和成分，不只检查字段数量。
    assert_eq!(serde_json::to_value(&content).unwrap(), expected);
    assert!(canonicalize_meanings(&mut content));
    assert_eq!(serde_json::to_value(&content).unwrap(), expected, "幂等");
}

#[test]
fn invalid_secondary_translation_is_rejected_and_its_text_is_not_changed() {
    let mut content = complete_content();
    let invalid = json!({"version": 2, "text": "test", "annotations": [
        {"type": "emphasis", "start": 0, "end": 8, "level": "strong"}
    ]});
    content.pos[0].senses[0].sentences[0].zh_translations[2].content =
        serde_json::from_value(invalid.clone()).unwrap();
    assert!(!canonicalize_meanings(&mut content));
    assert_eq!(
        serde_json::to_value(&content.pos[0].senses[0].sentences[0].zh_translations[2].content)
            .unwrap(),
        invalid
    );
}
