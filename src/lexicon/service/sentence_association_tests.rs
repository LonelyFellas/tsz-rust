use super::*;

use serde_json::{Value, json};

use crate::lexicon::dto::SentenceTargetMatchKindV3;

struct V3Fixture {
    snapshot: Value,
    entry_id: Uuid,
    pos_id: Uuid,
    sense_id: Uuid,
    form_ids: Vec<Uuid>,
    variant_ids: Vec<Uuid>,
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
                "dialect_rules": {
                    "spelling_mode": "unified",
                    "phonetic_mode": "unified"
                },
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
    }
}

#[test]
fn v3_target_resolves_automatic_associations() {
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
fn v3_discovery_candidates_repeat_the_same_form_inventory_for_every_base_form() {
    // 一个词形挂在两个 base form 下时会展开成两个候选。词形清单对最后一个候选是直接
    // move 过去的，前面的才克隆——顺序写反的话最后一个会拿到空清单。
    let pos_id = Uuid::now_v7();
    let publication_id = Uuid::now_v7();
    let first_base_id = Uuid::now_v7();
    let second_base_id = Uuid::now_v7();
    let matched_form_id = Uuid::now_v7();
    let matched_variant_id = Uuid::now_v7();
    let first_base_variant_id = Uuid::now_v7();
    let second_base_variant_id = Uuid::now_v7();

    let form = |id: Uuid, form_type: &str, variant_id: Uuid, spelling: &str, bases: Vec<Uuid>| {
        PublishedAssociationForm {
            id,
            form_type: form_type.to_owned(),
            base_form_ids: bases,
            variants: vec![PublishedAssociationVariant {
                id: variant_id,
                dialect: Dialect::Common,
                spelling: spelling.to_owned(),
                component_usages: Vec::new(),
            }],
        }
    };

    let target = PublishedAssociationTarget {
        schema_version: 3,
        id: Uuid::now_v7(),
        kind: EntryKind::Word,
        headword: "hang".to_owned(),
        pos: vec![PublishedAssociationPos {
            id: pos_id,
            pos: "verb".to_owned(),
            forms: vec![
                form(
                    first_base_id,
                    "base",
                    first_base_variant_id,
                    "hang",
                    vec![first_base_id],
                ),
                form(
                    second_base_id,
                    "base",
                    second_base_variant_id,
                    "hang",
                    vec![second_base_id],
                ),
                form(
                    matched_form_id,
                    "past_tense",
                    matched_variant_id,
                    "hung",
                    vec![first_base_id, second_base_id],
                ),
            ],
            senses: vec![PublishedAssociationSense {
                id: Uuid::now_v7(),
                level: "B1".to_owned(),
                gloss: "悬挂".to_owned(),
                component_usages: Vec::new(),
            }],
        }],
    };

    let candidates = target.sentence_discovery_candidates(
        publication_id,
        pos_id,
        matched_form_id,
        matched_variant_id,
        Some(SentenceTargetMatchEvidenceV3 {
            surface: "hung".to_owned(),
            normalized_surface: "hung".to_owned(),
            match_kind: SentenceTargetMatchKindV3::Word,
        }),
    );

    let [first, second] = candidates.as_slice() else {
        panic!("两个 base form 应展开成两个候选，实际 {}", candidates.len());
    };
    assert_eq!(first.base_form_id, first_base_id);
    assert_eq!(second.base_form_id, second_base_id);

    type CandidateFormRow<'a> = (Uuid, Uuid, WordFormTypeV3, Dialect, &'a str, Vec<Uuid>);
    fn inventory(candidate: &PublishedSentenceTargetCandidateV3) -> Vec<CandidateFormRow<'_>> {
        candidate
            .forms
            .iter()
            .map(|form| {
                (
                    form.form_id,
                    form.variant_id,
                    form.form_type,
                    form.dialect,
                    form.spelling.as_str(),
                    form.base_form_ids.clone(),
                )
            })
            .collect()
    }
    // 每个候选都要拿到完整的三条词形，最后一个不能因为被 move 走而空掉；
    // 逐字段比对，避免只看拼写时词形与变体错配也能蒙混过关。
    // base_form_ids 逐条声明所属变化组：两个原形各自成组，屈折形跨两组，
    // 调用方跨组改选词形时才有配套的 base form 可送。
    let expected = vec![
        (
            first_base_id,
            first_base_variant_id,
            WordFormTypeV3::Base,
            Dialect::Common,
            "hang",
            vec![first_base_id],
        ),
        (
            second_base_id,
            second_base_variant_id,
            WordFormTypeV3::Base,
            Dialect::Common,
            "hang",
            vec![second_base_id],
        ),
        (
            matched_form_id,
            matched_variant_id,
            WordFormTypeV3::PastTense,
            Dialect::Common,
            "hung",
            vec![first_base_id, second_base_id],
        ),
    ];
    assert_eq!(inventory(first), expected);
    assert_eq!(inventory(second), expected);
}

#[test]
fn v2_target_candidate_forms_carry_no_base_form_ids() {
    // 短语成分只接受 V3 发布的目标，V2 目标的词形一律不给可搭配的原形：调用方按「为空不可选」
    // 一条规则处理即可。候选行自身的 base_form_id 仍要有值——它是候选的身份（发现结果按它分组），
    // 只是不表示可用作成分。
    let publication_id = Uuid::now_v7();
    let pos_id = Uuid::now_v7();
    let base_form_id = Uuid::now_v7();
    let variant_id = Uuid::now_v7();
    let target = PublishedAssociationTarget {
        schema_version: 2,
        id: Uuid::now_v7(),
        kind: EntryKind::Word,
        headword: "location".to_owned(),
        pos: vec![PublishedAssociationPos {
            id: pos_id,
            pos: "noun".to_owned(),
            forms: vec![PublishedAssociationForm {
                id: base_form_id,
                form_type: "base".to_owned(),
                base_form_ids: vec![base_form_id],
                variants: vec![PublishedAssociationVariant {
                    id: variant_id,
                    dialect: Dialect::Common,
                    spelling: "location".to_owned(),
                    component_usages: Vec::new(),
                }],
            }],
            senses: vec![PublishedAssociationSense {
                id: Uuid::now_v7(),
                level: "A1".to_owned(),
                gloss: "位置".to_owned(),
                component_usages: Vec::new(),
            }],
        }],
    };

    let candidates = target.sentence_discovery_candidates(
        publication_id,
        pos_id,
        base_form_id,
        variant_id,
        Some(SentenceTargetMatchEvidenceV3 {
            surface: "location".to_owned(),
            normalized_surface: "location".to_owned(),
            match_kind: SentenceTargetMatchKindV3::Word,
        }),
    );

    let [candidate] = candidates.as_slice() else {
        panic!("V2 目标应展开成一条候选，实际 {}", candidates.len());
    };
    assert_eq!(
        candidate.base_form_id, base_form_id,
        "候选行身份仍取自 V2 的唯一原形"
    );
    assert_eq!(
        candidate.forms.len(),
        1,
        "词形清单本身照常给出，空的只是 base_form_ids：{:?}",
        candidate.forms
    );
    assert!(
        candidate
            .forms
            .iter()
            .all(|form| form.base_form_ids.is_empty()),
        "V2 目标的词形不给可搭配的原形：{:?}",
        candidate.forms
    );
}

#[test]
fn v3_snapshot_derives_form_group_bases_for_candidate_inventory() {
    // 三个原形 A、B、C 与一条过去式 P：G1 = {hi, P}、G2 = {lo, P}、G3 = {A}，C 不入任何组
    // （hi/lo 是 A、B 里 id 较大/较小的那个，id 大的组排前面，推导的原始顺序才是 [hi, lo]，
    // 排序断言才真的压在 sort 上）。分别钉住跨组并集与排序（P）、同一原形出现在两组时的
    // 去重（A）、无组原形的兜底（C），任何一条被"简化"掉，这里都会红。
    let mut fixture = v3_fixture(3);
    let publication_id = Uuid::now_v7();
    let [base_a, base_b, base_c] = fixture.form_ids[..] else {
        panic!("fixture should provide three base forms");
    };
    let past_id = Uuid::now_v7();
    let past_variant_id = Uuid::now_v7();
    fixture.snapshot["forms"]["pos"][0]["forms"]
        .as_array_mut()
        .expect("fixture forms should be an array")
        .push(json!({
            "id": past_id,
            "form_type": "past_tense",
            "regional_variants": {
                "mode": "common",
                "common": {
                    "id": past_variant_id,
                    "dialect": "common",
                    "spelling": "harboured",
                    "origin": "manual",
                    "pronunciations": []
                }
            }
        }));
    let group = |is_regular: bool, members: &[Uuid]| {
        json!({
            "id": Uuid::now_v7(),
            "is_regular": is_regular,
            "members": members
                .iter()
                .map(|form_id| json!({"id": Uuid::now_v7(), "form_id": form_id}))
                .collect::<Vec<_>>()
        })
    };
    let (lo, hi) = (base_a.min(base_b), base_a.max(base_b));
    fixture.snapshot["forms"]["pos"][0]["form_groups"] = json!([
        group(true, &[hi, past_id]),
        group(false, &[lo, past_id]),
        group(true, &[base_a]),
    ]);

    let target = PublishedAssociationTarget::from_snapshot(fixture.snapshot, true)
        .expect("V3 snapshot with form groups should convert");
    let candidates = target.sentence_discovery_candidates(
        publication_id,
        fixture.pos_id,
        past_id,
        past_variant_id,
        Some(SentenceTargetMatchEvidenceV3 {
            surface: "harboured".to_owned(),
            normalized_surface: "harboured".to_owned(),
            match_kind: SentenceTargetMatchKindV3::Word,
        }),
    );

    let expected_past_bases = vec![lo, hi];
    let mut candidate_bases = candidates
        .iter()
        .map(|candidate| candidate.base_form_id)
        .collect::<Vec<_>>();
    candidate_bases.sort_unstable();
    assert_eq!(
        candidate_bases, expected_past_bases,
        "过去式挂在两个变化组下，应按原形各出一条候选"
    );

    let inventory = candidates
        .first()
        .expect("at least one candidate")
        .forms
        .iter()
        .map(|form| (form.form_id, form.base_form_ids.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        inventory[&past_id], expected_past_bases,
        "跨组并集且按 id 排序：分组给出的原始顺序是 [hi, lo]"
    );
    assert_eq!(
        inventory[&base_a],
        vec![base_a],
        "A 同时挂在两个组下（自己那组与 G3），去重后只留一条"
    );
    assert_eq!(inventory[&base_b], vec![base_b]);
    assert_eq!(inventory[&base_c], vec![base_c], "无组原形兜底指向自己");
    // 每条候选自己的 base 都在命中词形的 base_form_ids 里，「在列表内就沿用」才总能成立。
    assert!(
        candidates
            .iter()
            .all(|candidate| inventory[&past_id].contains(&candidate.base_form_id))
    );
}
