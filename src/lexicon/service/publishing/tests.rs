use super::*;
use serde_json::json;

fn draft_target() -> ResolvedRelationTargetRecord {
    let sense_id = Uuid::now_v7();
    ResolvedRelationTargetRecord {
        target_entry_id: Uuid::now_v7(),
        target_sense_id: sense_id,
        target_revision: 4,
        target_archived: false,
        target_removed: false,
        content_schema_version: 3,
        presentation_label: Some("bank".to_owned()),
        draft_meanings: json!({
            "sense_groups": [],
            "pos": [{
                "pos_id": Uuid::now_v7(), "grammar_structures": [],
                "senses": [{
                    "id": sense_id, "sub_pos": "N-COUNT", "level": "A1",
                    "depends_on_context": false,
                    "definitions": [{
                        "definition_mode": "zh_definition", "id": Uuid::now_v7(),
                        "content_id": Uuid::now_v7(), "level": "A1",
                        "content": {"version": 2, "text": "银行", "annotations": []}
                    }],
                    "sentences": [], "relations": [],
                    "component_usages": [{
                        "state": "unresolved", "id": Uuid::now_v7(), "literal": "bank"
                    }]
                }]
            }]
        }),
        target_publication_id: None,
        published_snapshot: None,
        published_revision: None,
    }
}

#[test]
fn draft_reference_reads_native_meanings_and_retains_scope() {
    let record = draft_target();
    let target = relation_target_snapshot(&record).unwrap();
    assert_eq!(target.headword, "bank");
    assert_eq!(target.gloss, "银行");
    assert_eq!(target.target_revision, 4);
    assert_eq!(target.target_publication_id, None);
    assert_eq!(
        target.target_content_scope,
        PublicationTargetContentScope::Draft
    );
    assert!(target.available);
}

#[test]
fn draft_reference_does_not_make_archived_removed_or_missing_senses_available() {
    for unavailable in ["archived", "removed", "missing"] {
        let mut record = draft_target();
        match unavailable {
            "archived" => record.target_archived = true,
            "removed" => record.target_removed = true,
            "missing" => record.target_sense_id = Uuid::now_v7(),
            _ => unreachable!(),
        }
        let target = relation_target_snapshot(&record).unwrap();
        assert!(!target.available, "{unavailable}");
        if unavailable == "missing" {
            assert!(target.gloss.is_empty());
        }
    }
}

#[test]
fn draft_reference_rejects_legacy_content_schema_without_fallback() {
    let mut record = draft_target();
    record.content_schema_version = 2;
    assert!(matches!(
        relation_target_snapshot(&record),
        Err(LexiconServiceError::UnsupportedSchemaVersion(2))
    ));
}
