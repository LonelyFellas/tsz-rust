use super::*;
use serde_json::json;

fn native_meanings() -> DraftMeaningsStepContentV3 {
    let translation_ids = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    let rich = json!({"version": 2, "text": "test", "annotations": []});
    serde_json::from_value(json!({
        "sense_groups": [],
        "pos": [{
            "pos_id": Uuid::now_v7(), "grammar_structures": [],
            "senses": [{
                "id": Uuid::now_v7(), "sub_pos": "", "level": "A1",
                "depends_on_context": false, "definitions": [], "relations": [],
                "component_usages": [{"state": "unresolved", "id": Uuid::now_v7(), "literal": "test"}],
                "sentences": [{
                    "id": Uuid::now_v7(), "level": "A1",
                    "en_text": {"mode": "unified", "common": {
                        "id": Uuid::now_v7(), "origin": "manual", "value": rich
                    }},
                    "zh_text_id": translation_ids[1], "zh_text": rich,
                    "zh_translations": [
                        {"id": translation_ids[0], "band": "word_for_word", "language": "zh", "content": rich},
                        {"id": translation_ids[1], "band": "balanced_fluency", "language": "zh", "content": rich},
                        {"id": translation_ids[2], "band": "adapted_creation", "language": "zh", "content": rich}
                    ],
                    "links": [], "associations_state": "resolved", "associations": [{
                        "state": "pending", "id": Uuid::now_v7(), "association_schema_version": 3,
                        "source_dialect": "common", "source_segments": [{"start": 0, "end": 4, "surface": "test"}],
                        "origin": "manual", "pending_target_kind": "word", "pending_target_headword": "test"
                    }]
                }]
            }]
        }]
    })).unwrap()
}

#[test]
fn native_nodes_include_every_translation_and_component_but_not_alias_or_read_projection() {
    let content = native_meanings();
    let sense = &content.pos[0].senses[0];
    let sentence = &sense.sentences[0];
    let nodes = proposed_meaning_nodes(&content);
    // sense + sentence + English variant + three translations + component.
    assert_eq!(nodes.len(), 7);
    for translation in &sentence.zh_translations {
        let found = nodes
            .iter()
            .filter(|node| node.id == translation.id)
            .collect::<Vec<_>>();
        assert_eq!(found.len(), 1, "主译文别名不应产生重复节点");
        assert_eq!(found[0].parent_node_id, Some(sentence.id));
        assert_eq!(found[0].node_role, SENTENCE_TRANSLATION_ROLE);
        assert!(!found[0].stable_slot);
    }
    let component = nodes
        .iter()
        .find(|node| node.node_type == "phrase_component_usage")
        .unwrap();
    assert_eq!(component.parent_node_id, Some(sense.id));
    assert_eq!(component.node_role, PHRASE_COMPONENT_USAGE_ROLE);
    assert!(!component.stable_slot);
}

#[test]
fn translation_collision_with_another_node_is_not_silently_deduplicated() {
    let mut content = native_meanings();
    let sense = &mut content.pos[0].senses[0];
    let colliding_id = sense.id;
    sense.sentences[0].zh_translations[0].id = colliding_id;
    let proposed = proposed_meaning_nodes(&content);
    let issues = validate_node_identities(
        Uuid::now_v7(),
        &DraftFormsStepContentV3::default(),
        &proposed,
        &[],
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue.node_id == colliding_id && issue.code == "node_id_reused")
    );
}
