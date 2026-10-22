use super::*;
use serde_json::json;

fn legacy() -> DraftFormsStepContentV3 {
    serde_json::from_value(json!({"pos":[{"pos_id":Uuid::from_u128(1),"pos":"noun","forms":[{"id":Uuid::from_u128(2),"form_type":"base","regional_variants":{"mode":"common","common":{"id":Uuid::from_u128(3),"dialect":"common","spelling":"job","origin":"manual","is_regular":true,"pronunciations":[{"id":Uuid::from_u128(4),"dict_phonetic":"dʒɒb","actual_pron":"dʒɒb","style":"citation"}],"component_usages":[]}}}],"form_groups":[{"id":Uuid::from_u128(5),"is_regular":true,"scope":"general","dialect_rules":{"spelling_mode":"unified","phonetic_mode":"unified"},"members":[{"id":Uuid::from_u128(6),"form_id":Uuid::from_u128(2)}]}]}]})).unwrap()
}

fn shared_pair() -> SharedFormsStepContentV3 {
    let mut shared = legacy().shared_representation().unwrap();
    let mut verb = shared.pos[0].clone();
    verb.pos_id = Uuid::from_u128(11);
    verb.pos = "verb".into();
    verb.forms[0].id = Uuid::from_u128(12);
    let FormRegionalBindingsV3::Common { common } = &mut verb.forms[0].regional_bindings else {
        unreachable!()
    };
    common.id = Uuid::from_u128(13);
    common.pronunciations[0].id = Uuid::from_u128(14);
    common.is_regular = false;
    verb.form_groups[0].id = Uuid::from_u128(15);
    verb.form_groups[0].members[0].id = Uuid::from_u128(16);
    verb.form_groups[0].members[0].form_id = verb.forms[0].id;
    shared.pos.push(verb);
    shared
}

#[test]
fn reassignment_capability_is_negotiated_separately_from_shared_v1() {
    for header in [None, Some("v1"), Some("future")] {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-tsz-form-contents", "v1".parse().unwrap());
        if let Some(value) = header {
            headers.insert("x-tsz-form-content-reassignment", value.parse().unwrap());
        }
        let mut response = json!({"word": {"schema_version":3,"forms":legacy(),"capabilities":{"form_content_reassignment":true}}});
        project_response(&headers, &mut response).unwrap();
        let capabilities = &response["word"]["capabilities"];
        assert_eq!(capabilities["shared_form_contents"], true);
        if header == Some("v1") {
            assert_eq!(capabilities["form_content_reassignment"], true);
        } else {
            assert!(capabilities.get("form_content_reassignment").is_none());
        }
    }
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-tsz-form-content-reassignment", "v1".parse().unwrap());
    let mut response = json!({"word":{"schema_version":3,"forms":legacy(),"capabilities":{"form_content_reassignment":true}}});
    project_response(&headers, &mut response).unwrap();
    assert!(
        response["word"]["capabilities"]
            .get("form_content_reassignment")
            .is_none()
    );
}

#[test]
fn legacy_adapter_preserves_reference_tree_without_merging_spelling() {
    let legacy = legacy();
    let shared = legacy.shared_representation().unwrap();
    let restored = shared.clone().resolve().unwrap();
    assert_eq!(legacy.pos, restored.pos);
    assert_eq!(restored.shared_representation().unwrap(), shared);
    assert!(super::super::v3_contract::validate_forms(&restored, StepSaveIntent::Save).is_empty());
}

#[test]
fn shared_content_has_distinct_binding_nodes_and_survives_snapshot_roundtrip() {
    let mut shared = shared_pair();
    let FormContentRegionalVariantsV3::Common { common } =
        &mut shared.contents[0].regional_variants
    else {
        unreachable!()
    };
    common.pronunciations[0].actual_pron = "changed".into();
    let resolved = shared.resolve().unwrap();
    assert!(super::super::v3_contract::validate_forms(&resolved, StepSaveIntent::Save).is_empty());
    let sources =
        super::super::v3_projection::form_variant_sources(Uuid::from_u128(100), &resolved).unwrap();
    assert_eq!(sources.len(), 4);
    assert_ne!(sources[0].variant_id, sources[2].variant_id);
    let encoded = serde_json::to_value(&resolved).unwrap();
    assert_eq!(encoded["contents"].as_array().unwrap().len(), 1);
    assert!(
        encoded["pos"][0]["forms"][0]
            .get("regional_variants")
            .is_none()
    );
    let decoded: DraftFormsStepContentV3 = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, resolved);
    for (index, expected_regular) in [true, false].into_iter().enumerate() {
        let WordRegionalVariantsV3::Common { common } =
            &decoded.pos[index].forms[0].regional_variants
        else {
            unreachable!()
        };
        assert_eq!(common.pronunciations[0].actual_pron, "changed");
        assert_eq!(common.is_regular, Some(expected_regular));
        assert_eq!(common.id, Uuid::from_u128(if index == 0 { 3 } else { 13 }));
        assert_eq!(
            common.pronunciations[0].id,
            Uuid::from_u128(if index == 0 { 4 } else { 14 })
        );
    }
}

#[test]
fn pronunciation_mapping_order_does_not_change_content_order_or_sharing() {
    let mut shared = shared_pair();
    let FormContentRegionalVariantsV3::Common { common } =
        &mut shared.contents[0].regional_variants
    else {
        unreachable!()
    };
    let mut second = common.pronunciations[0].clone();
    second.id = Uuid::from_u128(40);
    second.actual_pron = "second".into();
    common.pronunciations.push(second);
    for (index, pos) in shared.pos.iter_mut().enumerate() {
        let FormRegionalBindingsV3::Common { common } = &mut pos.forms[0].regional_bindings else {
            unreachable!()
        };
        common.pronunciations.push(FormPronunciationBindingV3 {
            id: Uuid::from_u128(50 + index as u128),
            content_slot_id: Uuid::from_u128(40),
        });
        if index == 1 {
            common.pronunciations.reverse();
        }
    }
    let resolved = shared.resolve().unwrap();
    for pos in &resolved.pos {
        let WordRegionalVariantsV3::Common { common } = &pos.forms[0].regional_variants else {
            unreachable!()
        };
        assert_eq!(common.pronunciations[1].actual_pron, "second");
    }
    assert_eq!(resolved.shared_representation().unwrap().contents.len(), 1);
}

#[test]
fn shared_binding_component_usages_may_be_omitted_but_not_null() {
    let mut payload = serde_json::to_value(shared_pair()).unwrap();
    let variant = &mut payload["pos"][0]["forms"][0]["regional_bindings"]["common"];
    variant.as_object_mut().unwrap().remove("component_usages");
    let decoded: SharedFormsStepContentV3 = serde_json::from_value(payload.clone()).unwrap();
    assert!(decoded.resolve().is_ok());
    payload["pos"][0]["forms"][0]["regional_bindings"]["common"]["component_usages"] =
        serde_json::Value::Null;
    assert!(serde_json::from_value::<SharedFormsStepContentV3>(payload).is_err());
}

#[test]
fn pronunciation_correspondence_roundtrips_and_rejects_changed_parent_or_region() {
    let mut shared = shared_pair();
    let FormContentRegionalVariantsV3::Common { common } = &shared.contents[0].regional_variants
    else {
        unreachable!()
    };
    shared.contents[0].pronunciation_slots = vec![FormPronunciationSlotsV3 {
        id: Uuid::from_u128(100),
        common: Some(FormPronunciationSlotV3 {
            id: common.pronunciations[0].id,
            variant_slot_id: common.id,
        }),
        uk: Some(FormPronunciationSlotV3 {
            id: Uuid::from_u128(101),
            variant_slot_id: Uuid::from_u128(102),
        }),
        us: None,
    }];
    let decoded = shared.clone().resolve().unwrap();
    assert_eq!(decoded.shared_representation().unwrap(), shared);
    let encoded = serde_json::to_value(&decoded).unwrap();
    assert_eq!(
        serde_json::from_value::<DraftFormsStepContentV3>(encoded).unwrap(),
        decoded
    );
    let mut invalid = shared.clone();
    invalid.contents[0].pronunciation_slots[0]
        .common
        .as_mut()
        .unwrap()
        .variant_slot_id = Uuid::from_u128(999);
    assert!(invalid.resolve().is_err());
    let mut invalid = shared.clone();
    invalid.contents[0].pronunciation_slots[0].uk =
        invalid.contents[0].pronunciation_slots[0].common.clone();
    assert!(invalid.resolve().is_err());
    let mut invalid = shared;
    invalid.contents[0].pronunciation_slots[0].id = invalid.contents[0].id;
    assert!(invalid.resolve().is_err());
}

#[test]
fn mappings_reject_duplicate_reference_ids_dangling_slots_and_orphans() {
    let shared = shared_pair();
    let mut invalid = shared.clone();
    let FormRegionalBindingsV3::Common { common } = &mut invalid.pos[1].forms[0].regional_bindings
    else {
        unreachable!()
    };
    common.pronunciations[0].id = Uuid::from_u128(4);
    assert!(invalid.resolve().is_err());
    let mut invalid = shared.clone();
    let FormRegionalBindingsV3::Common { common } = &mut invalid.pos[1].forms[0].regional_bindings
    else {
        unreachable!()
    };
    common.content_slot_id = Uuid::from_u128(999);
    assert!(invalid.resolve().is_err());
    let mut invalid = shared;
    invalid.pos.clear();
    assert!(invalid.resolve().is_err());
}
