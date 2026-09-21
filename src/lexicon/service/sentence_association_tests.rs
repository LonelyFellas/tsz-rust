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
            "publication": {"mode": "native"},
            "pronunciation_normalization_version": "nfkc_trim_lower_v1"
        },
        "forms": {
            "pos": [{
                "pos_id": pos_id,
                "pos": "noun",
                "forms": forms,
                "form_groups": form_ids.iter().map(|id| json!({
                    "id": Uuid::now_v7(), "is_regular": true, "scope": "general",
                    "dialect_rules": {"spelling_mode": "unified", "phonetic_mode": "unified"},
                    "members": [{"id": Uuid::now_v7(), "form_id": id}]
                })).collect::<Vec<_>>()
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
fn native_target_readers_preserve_sense_components_with_resolved_sentence_projections() {
    let mut fixture = v3_fixture(1);
    fixture.snapshot["kind"] = json!("phrase");
    let component_id = Uuid::now_v7();
    let sense = &mut fixture.snapshot["meanings"]["pos"][0]["senses"][0];
    sense["component_usages"] = json!([{
        "state": "unresolved", "id": component_id, "literal": "harbour"
    }]);
    sense["sentences"][0]["associations_state"] = json!("resolved");
    sense["sentences"][0]["associations"] = json!([{
        "state": "pending",
        "id": Uuid::now_v7(),
        "association_schema_version": 3,
        "source_dialect": "common",
        "source_segments": [{"start": 4, "end": 11, "surface": "harbour"}],
        "origin": "auto",
        "pending_target_kind": "word",
        "pending_target_headword": "harbour"
    }]);
    let publication_id = Uuid::now_v7();
    let reference = ResolvedSenseTargetRecord {
        target_entry_id: fixture.entry_id,
        target_sense_id: fixture.sense_id,
        target_publication_id: publication_id,
        target_revision: 7,
        snapshot: fixture.snapshot.clone(),
    };
    assert_eq!(
        published_sense_snapshot(&reference).unwrap(),
        ("harbour".to_owned(), "港口".to_owned())
    );
    let target = PublishedAssociationTarget::from_snapshot(fixture.snapshot, true).unwrap();
    let candidates = target.sentence_discovery_candidates(
        Some(publication_id),
        fixture.pos_id,
        fixture.form_ids[0],
        fixture.variant_ids[0],
        None,
    );
    assert_eq!(candidates.len(), 1);
    let sense = &candidates[0].senses[0];
    assert_eq!(sense.sense_id, fixture.sense_id);
    assert_eq!(sense.gloss, "港口");
    assert_eq!(
        sense.component_usages,
        vec![PhraseComponentUsageV3::Unresolved {
            id: component_id,
            literal: "harbour".to_owned(),
        }]
    );
}

#[test]
fn native_gloss_uses_first_chinese_definition_and_never_falls_back_to_english() {
    let fixture = v3_fixture(1);
    let mut word: AdminWordV3 = serde_json::from_value(fixture.snapshot).unwrap();
    let sense = &mut word.meanings.pos[0].senses[0];
    let english = serde_json::from_value(json!({
        "definition_mode": "en_definition", "id": Uuid::now_v7(), "level": "A1",
        "content": {"mode": "unified", "common": {
            "id": Uuid::now_v7(), "origin": "manual", "value": rich_text("a port")
        }}
    }))
    .unwrap();
    sense.definitions.insert(0, english);
    assert_eq!(published_sense_gloss(sense), "港口");
    sense.definitions.truncate(1);
    assert_eq!(published_sense_gloss(sense), "");
    sense.definitions.clear();
    assert_eq!(published_sense_gloss(sense), "");
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
            allowed_sense_ids: Vec::new(),
            variants: vec![PublishedAssociationVariant {
                id: variant_id,
                dialect: Dialect::Common,
                spelling: spelling.to_owned(),
                component_usages: Vec::new(),
            }],
        }
    };

    let target = PublishedAssociationTarget {
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
        Some(publication_id),
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
                    form.form_type.clone(),
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
            "base".to_owned(),
            Dialect::Common,
            "hang",
            vec![first_base_id],
        ),
        (
            second_base_id,
            second_base_variant_id,
            "base".to_owned(),
            Dialect::Common,
            "hang",
            vec![second_base_id],
        ),
        (
            matched_form_id,
            matched_variant_id,
            "past_tense".to_owned(),
            Dialect::Common,
            "hung",
            vec![first_base_id, second_base_id],
        ),
    ];
    assert_eq!(inventory(first), expected);
    assert_eq!(inventory(second), expected);
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
            "scope": "general",
            "dialect_rules": {"spelling_mode": "unified", "phonetic_mode": "unified"},
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
        Some(publication_id),
        fixture.pos_id,
        past_id,
        past_variant_id,
        Some(SentenceTargetMatchEvidenceV3 {
            surface: "harboured".to_owned(),
            normalized_surface: "harboured".to_owned(),
            match_kind: SentenceTargetMatchKindV3::Word,
        }),
    );

    // 配置顺序来自组和组内成员，不是 forms 的创建顺序；共享词形只出现一次。
    // 未入组的历史原形仍保留在末尾，不能因展示排序改变候选集合。
    for candidate in &candidates {
        assert_eq!(
            candidate
                .forms
                .iter()
                .map(|form| form.form_id)
                .collect::<Vec<_>>(),
            vec![hi, past_id, lo, base_c],
            "candidate inventory must follow configured group/member order"
        );
    }
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

#[test]
fn draft_and_published_candidates_follow_member_reordering_without_changing_identity() {
    use super::v3::{ComponentTargetScope, ComponentTargetWord};

    let mut fixture = v3_fixture(3);
    let ids = fixture.form_ids.clone();
    // 同组配置与创建顺序相反；随后再调序，候选身份和词义范围必须保持不变。
    fixture.snapshot["forms"]["pos"][0]["form_groups"] = json!([{
        "id": Uuid::now_v7(), "is_regular": true, "scope": "general",
        "dialect_rules": {"spelling_mode": "unified", "phonetic_mode": "unified"},
        "members": ([ids[2], ids[0], ids[1]].map(|id| json!({"id": Uuid::now_v7(), "form_id": id})))
    }]);
    let mut word: AdminWordV3 = serde_json::from_value(fixture.snapshot).unwrap();
    let inventory = |word: &AdminWordV3, published: bool| {
        let target = if published {
            PublishedAssociationTarget::from_v3(word.clone()).unwrap()
        } else {
            PublishedAssociationTarget::from_component_target(ComponentTargetWord {
                id: word.id,
                kind: word.kind,
                label: word.presentation.label.clone(),
                forms: word.forms.clone(),
                meanings: word.meanings.clone(),
                scope: ComponentTargetScope::Draft { revision: 1 },
            })
            .unwrap()
        };
        target.sentence_discovery_candidates(
            None,
            fixture.pos_id,
            ids[0],
            fixture.variant_ids[0],
            None,
        )[0]
        .forms
        .iter()
        .map(|form| {
            (
                form.form_id,
                form.variant_id,
                form.base_form_ids.clone(),
                form.allowed_sense_ids.clone(),
            )
        })
        .collect::<Vec<_>>()
    };
    let before = inventory(&word, false);
    assert_eq!(
        before.iter().map(|form| form.0).collect::<Vec<_>>(),
        vec![ids[2], ids[0], ids[1]]
    );
    assert_eq!(inventory(&word, true), before);
    word.forms.pos[0].form_groups[0].members.swap(0, 2);
    let after = inventory(&word, false);
    assert_eq!(
        after,
        vec![before[2].clone(), before[1].clone(), before[0].clone()]
    );
    assert_eq!(inventory(&word, true), after);
    assert_eq!(
        word.forms.pos[0]
            .forms
            .iter()
            .map(|form| form.id)
            .collect::<Vec<_>>(),
        ids
    );
}

#[test]
fn dedicated_forms_limit_candidates_and_saved_targets_to_bound_senses_in_the_same_pos() {
    use crate::lexicon::dto::TextLinkV3;
    use crate::lexicon::service::{
        text_links,
        v3::{ComponentTargetScope, ComponentTargetWord, phrase_component_matches_target},
    };

    let mut fixture = v3_fixture(3);
    let group_id = fixture.snapshot["forms"]["pos"][0]["form_groups"][1]["id"].clone();
    fixture.snapshot["forms"]["pos"][0]["form_groups"][1]["scope"] = json!("dedicated");
    // 第三组没有绑定词义（允许编辑中的草稿），不能回退全量。
    fixture.snapshot["forms"]["pos"][0]["form_groups"][2]["scope"] = json!("dedicated");
    let first_sense = fixture.snapshot["meanings"]["pos"][0]["senses"][0].clone();
    let mut sense_ids = vec![fixture.sense_id];
    for _ in 0..2 {
        let mut sense = first_sense.clone();
        let id = Uuid::now_v7();
        sense_ids.push(id);
        sense["id"] = json!(id);
        sense["form_group_id"] = group_id.clone();
        fixture.snapshot["meanings"]["pos"][0]["senses"]
            .as_array_mut()
            .unwrap()
            .push(sense);
    }
    let other_sense_id = Uuid::now_v7();
    let mut other_sense = first_sense;
    other_sense["id"] = json!(other_sense_id);
    fixture.snapshot["meanings"]["pos"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "pos_id": Uuid::now_v7(), "grammar_structures": [], "senses": [other_sense]
        }));
    let word: AdminWordV3 = serde_json::from_value(fixture.snapshot.clone()).unwrap();
    let target = PublishedAssociationTarget::from_snapshot(fixture.snapshot, true).unwrap();
    let candidates = target.sentence_discovery_candidates(
        None,
        fixture.pos_id,
        fixture.form_ids[1],
        fixture.variant_ids[1],
        None,
    );
    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    assert_eq!(
        candidate.senses.len(),
        3,
        "保留跨组切换所需词义，但不包含其他词性"
    );
    assert_eq!(
        candidate.forms[0].allowed_sense_ids.as_ref().unwrap(),
        &sense_ids
    );
    assert_eq!(
        candidate.forms[1].allowed_sense_ids.as_ref().unwrap(),
        &sense_ids[1..]
    );
    assert_eq!(candidate.forms[2].allowed_sense_ids, Some(vec![]));
    let wire = serde_json::to_value(&candidate.forms[2]).unwrap();
    assert_eq!(wire["allowed_sense_ids"], json!([]));

    let component_target = ComponentTargetWord {
        id: word.id,
        kind: word.kind,
        label: word.presentation.label.clone(),
        forms: word.forms.clone(),
        meanings: word.meanings.clone(),
        scope: ComponentTargetScope::Draft { revision: 7 },
    };
    // 正文/共享例句、成分保存与候选必须使用相同规则。
    for (index, form_id) in fixture.form_ids.iter().enumerate() {
        for sense_id in sense_ids.iter().chain(std::iter::once(&other_sense_id)) {
            let expected = *sense_id != other_sense_id
                && (index == 0 || (index == 1 && *sense_id != fixture.sense_id));
            assert_eq!(
                phrase_component_matches_target(
                    &component_target,
                    fixture.pos_id,
                    *form_id,
                    *sense_id,
                    *form_id,
                    fixture.variant_ids[index],
                    Dialect::Common,
                    "base".into(),
                    None,
                ),
                expected
            );
            let link: TextLinkV3 = serde_json::from_value(json!({
                "id": Uuid::now_v7(), "source_segments": [{"start":0,"end":7,"surface":"harbour"}],
                "target_word_id": word.id, "target_pos_id": fixture.pos_id,
                "target_base_form_id": form_id, "target_form_id": form_id,
                "target_variant_id": fixture.variant_ids[index], "target_sense_id": sense_id
            }))
            .unwrap();
            assert_eq!(
                text_links::shared_target_matches(&word.forms, &word.meanings, &link, "common"),
                expected
            );
        }
    }
}

#[test]
fn automatic_association_does_not_pick_an_unbound_dedicated_form() {
    let mut fixture = v3_fixture(2);
    fixture.snapshot["forms"]["pos"][0]["form_groups"][1]["scope"] = json!("dedicated");
    let target = PublishedAssociationTarget::from_snapshot(fixture.snapshot, true).unwrap();
    assert!(
        target
            .automatic_target(fixture.pos_id, &[fixture.variant_ids[1]])
            .is_none()
    );
    assert_eq!(
        target
            .automatic_target(fixture.pos_id, &fixture.variant_ids)
            .unwrap()
            .target_form_slot_id,
        Some(fixture.form_ids[0])
    );
}

#[test]
fn two_dedicated_groups_can_offer_the_same_sense() {
    let mut fixture = v3_fixture(2);
    let ids = fixture.snapshot["forms"]["pos"][0]["form_groups"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .map(|group| {
            group["scope"] = json!("dedicated");
            group["id"].clone()
        })
        .collect::<Vec<_>>();
    fixture.snapshot["meanings"]["pos"][0]["senses"][0]["form_group_ids"] = json!(ids);
    let target = PublishedAssociationTarget::from_snapshot(fixture.snapshot, true).unwrap();
    let candidates = target.sentence_discovery_candidates(
        None,
        fixture.pos_id,
        fixture.form_ids[0],
        fixture.variant_ids[0],
        None,
    );
    assert_eq!(candidates.len(), 1);
    for form in &candidates[0].forms {
        assert_eq!(form.allowed_sense_ids, Some(vec![fixture.sense_id]));
    }
}

/// TASK#58：英美结构切换（common ↔ uk_us）后，变体实例 id 会换掉，但引用在业务上绑的是
/// 「词形 + 方言侧」，应仍成立。这里覆盖共享例句标注引用与短语成分类引用的判定。
mod dialect_structure_drift {
    use super::*;
    use crate::lexicon::dto::TextLinkV3;
    use crate::lexicon::service::text_links;
    use crate::lexicon::service::v3::{
        ComponentTargetScope, ComponentTargetWord, phrase_component_matches_target,
    };

    /// 把 fixture 第 0 个词形从 common 结构改写成 uk_us：uk 复用原 common 的 id（同侧），
    /// us 另给新 id；拼写沿用原 common 值，模拟「拆成英美、拼写暂时共用」。
    fn to_uk_us(snapshot: &mut Value, form_index: usize, common_id: Uuid) -> (Uuid, Uuid) {
        let uk_id = common_id;
        let us_id = Uuid::now_v7();
        snapshot["forms"]["pos"][0]["forms"][form_index]["regional_variants"] = json!({
            "mode": "uk_us",
            "uk": {
                "id": uk_id, "dialect": "uk", "spelling": "harbour",
                "origin": "manual", "pronunciations": []
            },
            "us": {
                "id": us_id, "dialect": "us", "spelling": "harbor",
                "origin": "manual", "pronunciations": []
            }
        });
        (uk_id, us_id)
    }

    fn component_target(snapshot: &Value) -> (AdminWordV3, ComponentTargetWord) {
        let word: AdminWordV3 = serde_json::from_value(snapshot.clone()).unwrap();
        let target = ComponentTargetWord {
            id: word.id,
            kind: word.kind,
            label: word.presentation.label.clone(),
            forms: word.forms.clone(),
            meanings: word.meanings.clone(),
            scope: ComponentTargetScope::Draft { revision: 0 },
        };
        (word, target)
    }

    #[test]
    fn shared_sentence_reference_survives_common_to_uk_us() {
        let fixture = v3_fixture(1);
        let form_id = fixture.form_ids[0];
        let common_id = fixture.variant_ids[0];
        // 引用原 common 变体；标注片段与 common 拼写一致。
        let link: TextLinkV3 = serde_json::from_value(json!({
            "id": Uuid::now_v7(),
            "source_segments": [{"start":0,"end":7,"surface":"harbour"}],
            "target_word_id": fixture.entry_id, "target_pos_id": fixture.pos_id,
            "target_base_form_id": form_id, "target_form_id": form_id,
            "target_variant_id": common_id, "target_sense_id": fixture.sense_id
        }))
        .unwrap();
        let word: AdminWordV3 = serde_json::from_value(fixture.snapshot.clone()).unwrap();
        assert!(text_links::shared_target_matches(
            &word.forms,
            &word.meanings,
            &link,
            "common"
        ));

        // 切成 uk_us：uk 侧复用原 common id，引用应仍成立（id 快速路径命中）。
        let mut drifted = fixture.snapshot.clone();
        to_uk_us(&mut drifted, 0, common_id);
        let word: AdminWordV3 = serde_json::from_value(drifted).unwrap();
        assert!(
            text_links::shared_target_matches(&word.forms, &word.meanings, &link, "common"),
            "结构换成 uk_us 后，指向原 common 变体的引用应仍成立"
        );
    }

    #[test]
    fn shared_sentence_reference_survives_id_change_common_to_uk_us() {
        let fixture = v3_fixture(1);
        let form_id = fixture.form_ids[0];
        let common_id = fixture.variant_ids[0];
        let link: TextLinkV3 = serde_json::from_value(json!({
            "id": Uuid::now_v7(),
            "source_segments": [{"start":0,"end":7,"surface":"harbour"}],
            "target_word_id": fixture.entry_id, "target_pos_id": fixture.pos_id,
            "target_base_form_id": form_id, "target_form_id": form_id,
            "target_variant_id": common_id, "target_sense_id": fixture.sense_id
        }))
        .unwrap();
        // 结构换成 uk_us 且两侧都是全新 id：id 快速路径必然落空，靠方言侧重解析成立。
        let mut drifted = fixture.snapshot.clone();
        drifted["forms"]["pos"][0]["forms"][0]["regional_variants"] = json!({
            "mode": "uk_us",
            "uk": {"id": Uuid::now_v7(), "dialect": "uk", "spelling": "harbour",
                   "origin": "manual", "pronunciations": []},
            "us": {"id": Uuid::now_v7(), "dialect": "us", "spelling": "harbor",
                   "origin": "manual", "pronunciations": []}
        });
        let word: AdminWordV3 = serde_json::from_value(drifted).unwrap();
        assert!(
            text_links::shared_target_matches(&word.forms, &word.meanings, &link, "common"),
            "变体 id 全换后，按方言侧拼写重解析应仍成立"
        );
    }

    #[test]
    fn shared_sentence_reference_survives_uk_us_to_common() {
        let mut snapshot = v3_fixture(1).snapshot.clone();
        let form_id: Uuid =
            serde_json::from_value(snapshot["forms"]["pos"][0]["forms"][0]["id"].clone()).unwrap();
        let sense_id: Uuid =
            serde_json::from_value(snapshot["meanings"]["pos"][0]["senses"][0]["id"].clone())
                .unwrap();
        let entry_id: Uuid = serde_json::from_value(snapshot["id"].clone()).unwrap();
        let pos_id: Uuid =
            serde_json::from_value(snapshot["forms"]["pos"][0]["pos_id"].clone()).unwrap();
        // 先造成 uk_us 结构，引用 uk 侧。
        let (uk_id, _us_id) = to_uk_us(&mut snapshot, 0, Uuid::now_v7());
        let link: TextLinkV3 = serde_json::from_value(json!({
            "id": Uuid::now_v7(),
            "source_segments": [{"start":0,"end":7,"surface":"harbour"}],
            "target_word_id": entry_id, "target_pos_id": pos_id,
            "target_base_form_id": form_id, "target_form_id": form_id,
            "target_variant_id": uk_id, "target_sense_id": sense_id
        }))
        .unwrap();
        // 再合回 common，且 common 换新 id：引用落到 common 侧，仍成立。
        snapshot["forms"]["pos"][0]["forms"][0]["regional_variants"] = json!({
            "mode": "common",
            "common": {"id": Uuid::now_v7(), "dialect": "common", "spelling": "harbour",
                       "origin": "manual", "pronunciations": []}
        });
        let word: AdminWordV3 = serde_json::from_value(snapshot).unwrap();
        assert!(
            text_links::shared_target_matches(&word.forms, &word.meanings, &link, "uk"),
            "合回 common 后，指向 uk 变体的引用应仍成立"
        );
    }

    #[test]
    fn phrase_component_reference_survives_common_to_uk_us() {
        let fixture = v3_fixture(1);
        let form_id = fixture.form_ids[0];
        let common_id = fixture.variant_ids[0];
        // uk_us 结构 + 指向 uk 侧（复用 common id）。
        let mut drifted = fixture.snapshot.clone();
        let (uk_id, _us_id) = to_uk_us(&mut drifted, 0, common_id);
        let (_word, target) = component_target(&drifted);
        assert!(phrase_component_matches_target(
            &target,
            fixture.pos_id,
            form_id,
            fixture.sense_id,
            form_id,
            uk_id,
            Dialect::Uk,
            "base".into(),
            None,
        ));
    }

    #[test]
    fn phrase_component_reference_survives_form_type_change() {
        let fixture = v3_fixture(1);
        let form_id = fixture.form_ids[0];
        let variant_id = fixture.variant_ids[0];
        // 引用时记的是 base 类型；把当前 form 类型改成过去式应仍成立（保护式放开）。
        let mut changed = fixture.snapshot.clone();
        changed["forms"]["pos"][0]["forms"][0]["form_type"] = json!("past_tense");
        let (_word, target) = component_target(&changed);
        assert!(
            phrase_component_matches_target(
                &target,
                fixture.pos_id,
                form_id,
                fixture.sense_id,
                form_id,
                variant_id,
                Dialect::Common,
                "base".into(),
                None,
            ),
            "词形类型漂移不应让成分引用失效"
        );
    }

    #[test]
    fn phrase_component_reference_still_fails_when_form_is_gone() {
        let fixture = v3_fixture(1);
        let form_id = fixture.form_ids[0];
        let variant_id = fixture.variant_ids[0];
        let (_word, target) = component_target(&fixture.snapshot);
        // 引用指向一个不存在的 form：应失效（防放宽过头）。
        assert!(
            !phrase_component_matches_target(
                &target,
                fixture.pos_id,
                form_id,
                fixture.sense_id,
                Uuid::now_v7(),
                variant_id,
                Dialect::Common,
                "base".into(),
                None,
            ),
            "目标词形不存在时引用必须失效"
        );
    }

    /// 显式 `target_dialect` 必须被尊重：标了 uk 侧就不认美式拼写（覆盖 `Some(Uk)`），
    /// 标了 us 侧就认美式拼写（覆盖 `Some(Us)`）；缺字段的存量引用仍按容忍口径。
    #[test]
    fn shared_sentence_reference_honors_explicit_target_dialect() {
        let fixture = v3_fixture(1);
        let form_id = fixture.form_ids[0];
        let mut drifted = fixture.snapshot.clone();
        // uk 侧拼 "harbour"（复用原 common id）、us 侧拼 "harbor"。
        let (_uk_id, _us_id) = to_uk_us(&mut drifted, 0, fixture.variant_ids[0]);
        let word: AdminWordV3 = serde_json::from_value(drifted).unwrap();
        let link = |surface: &str, target_dialect: Option<&str>, variant_id: Uuid| -> TextLinkV3 {
            let mut value = json!({
                "id": Uuid::now_v7(),
                "source_segments": [{"start":0,"end":surface.len(),"surface":surface}],
                "target_word_id": fixture.entry_id, "target_pos_id": fixture.pos_id,
                "target_base_form_id": form_id, "target_form_id": form_id,
                "target_variant_id": variant_id, "target_sense_id": fixture.sense_id
            });
            if let Some(dialect) = target_dialect {
                value["target_dialect"] = json!(dialect);
            }
            serde_json::from_value(value).unwrap()
        };
        assert!(
            !text_links::shared_target_matches(
                &word.forms,
                &word.meanings,
                &link("harbor", Some("uk"), Uuid::now_v7()),
                "common"
            ),
            "target_dialect=uk 时，只有美式拼写的引用应判失效"
        );
        // 用新 id 保证不进 id 快速路径，真正走到 `Some(Us)` 兜底分支。
        assert!(
            text_links::shared_target_matches(
                &word.forms,
                &word.meanings,
                &link("harbor", Some("us"), Uuid::now_v7()),
                "common"
            ),
            "target_dialect=us 时，美式拼写应命中"
        );
        assert!(
            text_links::shared_target_matches(
                &word.forms,
                &word.meanings,
                &link("harbor", None, Uuid::now_v7()),
                "common"
            ),
            "缺 target_dialect 的存量引用按任一侧拼写容忍匹配"
        );
        assert!(
            !text_links::shared_target_matches(
                &word.forms,
                &word.meanings,
                &link("harbourx", None, Uuid::now_v7()),
                "common"
            ),
            "拼写与任何一侧都对不上时必须失效（防放宽过头）"
        );
    }

    /// 变体实例 id 漂移后，只要 form / base / sense 还在，成分引用仍成立：
    /// 绑定坐标是「词形 + 方言侧」，实例 id 只作记录（见 `dialect_side_available` 说明）。
    #[test]
    fn phrase_component_reference_survives_variant_id_change() {
        let fixture = v3_fixture(1);
        let form_id = fixture.form_ids[0];
        let mut drifted = fixture.snapshot.clone();
        let (_uk_id, _us_id) = to_uk_us(&mut drifted, 0, fixture.variant_ids[0]);
        let (_word, target) = component_target(&drifted);
        assert!(
            phrase_component_matches_target(
                &target,
                fixture.pos_id,
                form_id,
                fixture.sense_id,
                form_id,
                Uuid::now_v7(),
                Dialect::Uk,
                "base".into(),
                None,
            ),
            "变体实例 id 漂移不应让成分引用失效"
        );
    }

    #[test]
    fn phrase_component_reference_still_fails_when_sense_is_gone() {
        let fixture = v3_fixture(1);
        let form_id = fixture.form_ids[0];
        let (_word, target) = component_target(&fixture.snapshot);
        assert!(
            !phrase_component_matches_target(
                &target,
                fixture.pos_id,
                form_id,
                Uuid::now_v7(),
                form_id,
                fixture.variant_ids[0],
                Dialect::Common,
                "base".into(),
                None,
            ),
            "目标词义不存在时引用必须失效"
        );
    }
}
