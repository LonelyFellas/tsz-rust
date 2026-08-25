use super::*;

use serde_json::{Value, json};

struct V3Fixture {
    snapshot: Value,
    entry_id: Uuid,
    pos_id: Uuid,
    sense_id: Uuid,
    form_ids: Vec<Uuid>,
    variant_ids: Vec<Uuid>,
    sentence_id: Uuid,
}

fn rich_text(text: &str) -> Value {
    json!({"version": 1, "text": text, "spans": [], "liaisons": []})
}

fn v3_fixture(form_count: usize) -> V3Fixture {
    let entry_id = Uuid::now_v7();
    let pos_id = Uuid::now_v7();
    let sense_id = Uuid::now_v7();
    let sentence_id = Uuid::now_v7();
    let form_ids = (0..form_count).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
    let variant_ids = (0..form_count).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
    let forms = form_ids
        .iter()
        .zip(&variant_ids)
        .map(|(form_id, variant_id)| {
            json!({
                "id": form_id,
                "form_type": "base",
                "regional_variants": {
                    "mode": "common",
                    "common": {
                        "id": variant_id,
                        "dialect": "common",
                        "spelling": "harbour",
                        "origin": "manual",
                        "pronunciations": []
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    let now = Utc::now();
    let snapshot = json!({
        "schema_version": 3,
        "id": entry_id,
        "language": "en",
        "kind": "word",
        "status": "published",
        "revision": 7,
        "lifecycle_revision": 2,
        "published_revision": 7,
        "has_unpublished_changes": false,
        "presentation": {
            "label": "harbour",
            "matched_surfaces": ["harbour"],
            "strategy_version": "sentence-association-test-v1"
        },
        "capabilities": {
            "publication": {"mode": "migration_canary", "whitelisted": true},
            "pronunciation_normalization_version": "nfkc_trim_lower_v1"
        },
        "forms": {
            "pos": [{
                "pos_id": pos_id,
                "pos": "noun",
                "forms": forms,
                "form_groups": []
            }]
        },
        "meanings": {
            "sense_groups": [],
            "pos": [{
                "pos_id": pos_id,
                "grammar_structures": [],
                "senses": [{
                    "id": sense_id,
                    "sub_pos": "N-COUNT",
                    "level": "A1",
                    "frequency": "50",
                    "depends_on_context": false,
                    "definitions": [{
                        "definition_mode": "zh_definition",
                        "id": Uuid::now_v7(),
                        "content_id": Uuid::now_v7(),
                        "level": "A1",
                        "content": rich_text("港口")
                    }],
                    "sentences": [{
                        "id": sentence_id,
                        "level": "A1",
                        "en_text": {
                            "mode": "unified",
                            "common": {
                                "id": Uuid::now_v7(),
                                "value": rich_text("The harbour is calm."),
                                "origin": "manual"
                            }
                        },
                        "zh_text_id": Uuid::now_v7(),
                        "zh_text": rich_text("港口很平静。"),
                        "links": []
                    }],
                    "relations": []
                }]
            }]
        },
        "completed_steps": ["basics", "forms", "meanings"],
        "max_reachable_step": "preview",
        "created_by": Uuid::now_v7(),
        "created_at": now,
        "updated_at": now,
        "published_at": now
    });
    V3Fixture {
        snapshot,
        entry_id,
        pos_id,
        sense_id,
        form_ids,
        variant_ids,
        sentence_id,
    }
}

#[test]
fn v3_target_resolves_auto_and_manual_associations() {
    let fixture = v3_fixture(1);
    let target = PublishedAssociationTarget::from_snapshot(fixture.snapshot, true)
        .expect("V3 publication snapshot should be supported");

    let automatic = target
        .automatic_target(fixture.pos_id, &fixture.variant_ids)
        .expect("unique V3 POS and sense should resolve");
    assert_eq!(automatic.target_entry_id, fixture.entry_id);
    assert_eq!(automatic.target_sense_id, fixture.sense_id);
    assert_eq!(automatic.target_form_slot_id, Some(fixture.form_ids[0]));
    assert_eq!(automatic.target_headword, "harbour");
    assert_eq!(automatic.target_gloss, "港口");
    assert_eq!(automatic.resolved_pos, "noun");
    assert_eq!(automatic.resolved_form_type.as_deref(), Some("base"));

    let manual = target
        .manual_target(fixture.sense_id, "harbour")
        .expect("V3 manual target should resolve");
    assert_eq!(manual.target_form_slot_id, Some(fixture.form_ids[0]));
}

#[test]
fn v3_same_surface_across_forms_keeps_association_without_guessing_form() {
    let fixture = v3_fixture(2);
    let target = PublishedAssociationTarget::from_snapshot(fixture.snapshot, true)
        .expect("V3 publication snapshot should be supported");

    let automatic = target
        .automatic_target(fixture.pos_id, &fixture.variant_ids)
        .expect("unique V3 POS and sense should still resolve");
    assert_eq!(automatic.target_form_slot_id, None);
    assert_eq!(automatic.resolved_form_type, None);

    let manual = target
        .manual_target(fixture.sense_id, "harbour")
        .expect("V3 manual target should still resolve");
    assert_eq!(manual.target_form_slot_id, None);
    assert_eq!(manual.resolved_form_type, None);
}

#[test]
fn v3_source_exposes_sentence_variants_to_the_manual_editor() {
    let fixture = v3_fixture(1);
    let word: AdminWordV3 =
        serde_json::from_value(fixture.snapshot).expect("fixture should be valid V3");
    let word = AdminWordAny::V3(Box::new(word));

    ensure_association_source_state(&word, 7, 2).expect("current V3 revisions should be accepted");
    let meanings = association_source_meanings(&word).expect("V3 meanings should adapt to V2 wire");
    let variant = sentence_variants(&meanings)
        .into_iter()
        .find(|variant| variant.sentence_id == fixture.sentence_id)
        .expect("V3 sentence should be editable");
    assert_eq!(variant.dialect, Dialect::Common);
    assert_eq!(variant.text, "The harbour is calm.");
}

#[test]
fn candidate_source_kinds_cover_v2_and_v3_publication_rows() {
    assert_eq!(
        crate::lexicon::sentence_association::association_form_source_kinds(false),
        &["form"]
    );
    assert_eq!(
        crate::lexicon::sentence_association::association_form_source_kinds(true),
        &["form", "form_variant"]
    );
}

#[test]
fn rollback_gate_rejects_v3_publication_targets() {
    let fixture = v3_fixture(1);
    let error = PublishedAssociationTarget::from_snapshot(fixture.snapshot, false)
        .expect_err("V3 target consumption must stop when the capability is disabled");

    assert!(matches!(error, LexiconServiceError::V3StorageUnavailable));
}

#[test]
fn v3_idempotent_replay_respects_the_rollback_gate() {
    let fixture = v3_fixture(1);
    let word: AdminWordV3 =
        serde_json::from_value(fixture.snapshot).expect("fixture should be valid V3");
    let response = AdminWordAnyEnvelope {
        word: AdminWordAny::V3(Box::new(word)),
    };

    let error = ensure_association_response_capability(&response, false)
        .expect_err("disabled V3 capability must reject a stored V3 response");
    assert!(matches!(error, LexiconServiceError::V3StorageUnavailable));
    ensure_association_response_capability(&response, true)
        .expect("enabled V3 capability should allow the same replay");
}

#[test]
fn v2_manual_slot_selection_keeps_legacy_first_match_behavior() {
    let first_form_id = Uuid::now_v7();
    let second_form_id = Uuid::now_v7();
    let second_variant_id = Uuid::now_v7();
    let sense_id = Uuid::now_v7();
    let pos_id = Uuid::now_v7();
    let target = PublishedAssociationTarget {
        schema_version: 2,
        id: Uuid::now_v7(),
        headword: "cut".to_owned(),
        pos: vec![PublishedAssociationPos {
            id: pos_id,
            pos: "verb".to_owned(),
            forms: vec![
                PublishedAssociationForm {
                    id: first_form_id,
                    form_type: "base".to_owned(),
                    variant_ids: vec![Uuid::now_v7()],
                    normalized_surfaces: vec!["cut".to_owned()],
                },
                PublishedAssociationForm {
                    id: second_form_id,
                    form_type: "past_tense".to_owned(),
                    variant_ids: vec![second_variant_id],
                    normalized_surfaces: vec!["cut".to_owned()],
                },
            ],
            senses: vec![PublishedAssociationSense {
                id: sense_id,
                gloss: "切".to_owned(),
            }],
        }],
    };

    let manual = target
        .manual_target(sense_id, "cut")
        .expect("legacy V2 manual target should resolve");
    assert_eq!(manual.target_form_slot_id, Some(first_form_id));

    let automatic = target
        .automatic_target(pos_id, &[second_variant_id])
        .expect("legacy V2 automatic target should resolve");
    assert_eq!(automatic.target_form_slot_id, Some(second_form_id));
    assert_eq!(automatic.resolved_form_type.as_deref(), Some("past_tense"));
}
