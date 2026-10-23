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

#[sqlx::test]
async fn concurrent_content_claim_returns_identity_conflict(pool: sqlx::PgPool) {
    use crate::lexicon::service::LexiconServiceError;
    let admin = Uuid::now_v7();
    sqlx::query("INSERT INTO admins(id,phone,password_hash,display_name) VALUES($1,$2,'hash','shared identity race')").bind(admin).bind(format!("race-{}", admin.simple())).execute(&pool).await.unwrap();
    let entries = [Uuid::now_v7(), Uuid::now_v7()];
    for entry in entries {
        sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,revision,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word',1,'{}',$2,$2)").bind(entry).bind(admin).execute(&pool).await.unwrap();
    }
    for collide_content in [true, false] {
        let mut shared = legacy().shared_representation().unwrap();
        shared.contents[0].id = Uuid::now_v7();
        let FormContentRegionalVariantsV3::Common { common } =
            &mut shared.contents[0].regional_variants
        else {
            unreachable!()
        };
        common.id = Uuid::now_v7();
        common.pronunciations.clear();
        let region = common.id;
        shared.pos[0].forms[0].content_id = shared.contents[0].id;
        let FormRegionalBindingsV3::Common { common } =
            &mut shared.pos[0].forms[0].regional_bindings
        else {
            unreachable!()
        };
        common.content_slot_id = region;
        common.pronunciations.clear();
        let forms = shared.resolve().unwrap();
        let content = forms.shared_representation().unwrap().contents[0].id;
        // Both requests pass the ownership read before the winner commits its claim.
        let mut winner = pool.begin().await.unwrap();
        let mut loser = pool.begin().await.unwrap();
        preflight_identities(&mut winner, entries[0], &forms, IdentityCheck::Draft, &[])
            .await
            .unwrap();
        let verified =
            preflight_identities(&mut loser, entries[1], &forms, IdentityCheck::Draft, &[])
                .await
                .unwrap();
        let winner_content = if collide_content {
            content
        } else {
            Uuid::now_v7()
        };
        if collide_content {
            sqlx::query(
                "INSERT INTO lexicon.v3_form_contents(id,entry_id,content) VALUES($1,$2,'{}')",
            )
            .bind(winner_content)
            .bind(entries[0])
            .execute(&mut *winner)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO lexicon.v3_form_content_slots(id,entry_id,content_id,role) VALUES($1,$2,$1,'content')").bind(winner_content).bind(entries[0]).execute(&mut *winner).await.unwrap();
        sqlx::query("INSERT INTO lexicon.v3_form_content_slots(id,entry_id,content_id,parent_slot_id,role) VALUES($1,$2,$3,$3,'common')").bind(region).bind(entries[0]).bind(winner_content).execute(&mut *winner).await.unwrap();
        winner.commit().await.unwrap();
        let error = persist(&mut loser, entries[1], &forms, verified)
            .await
            .err()
            .unwrap();
        assert!(
            matches!(error, LexiconServiceError::StableNodeIdChanged),
            "{error:?}"
        );
        loser.rollback().await.unwrap();
        let partial: i64 =
            sqlx::query_scalar("SELECT count(*) FROM lexicon.v3_form_contents WHERE entry_id=$1")
                .bind(entries[1])
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(partial, 0, "failed claims must roll back all writes");
    }
}

#[sqlx::test]
async fn opposing_content_claims_return_conflict_on_deadlock(pool: sqlx::PgPool) {
    use crate::lexicon::service::LexiconServiceError;
    let admin = Uuid::now_v7();
    sqlx::query("INSERT INTO admins(id,phone,password_hash,display_name) VALUES($1,$2,'hash','shared identity deadlock')").bind(admin).bind(format!("deadlock-{}", admin.simple())).execute(&pool).await.unwrap();
    let entries = [Uuid::now_v7(), Uuid::now_v7()];
    for entry in entries {
        sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,revision,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word',1,'{}',$2,$2)").bind(entry).bind(admin).execute(&pool).await.unwrap();
    }
    let mut shared = shared_pair();
    shared.contents.push(shared.contents[0].clone());
    for (content, pos) in shared.contents.iter_mut().zip(&mut shared.pos) {
        content.id = Uuid::now_v7();
        let FormContentRegionalVariantsV3::Common { common } = &mut content.regional_variants
        else {
            unreachable!()
        };
        common.id = Uuid::now_v7();
        common.pronunciations.clear();
        pos.forms[0].content_id = content.id;
        let FormRegionalBindingsV3::Common { common: binding } =
            &mut pos.forms[0].regional_bindings
        else {
            unreachable!()
        };
        binding.content_slot_id = common.id;
        binding.pronunciations.clear();
    }
    let mut reversed = shared.clone();
    reversed.contents.reverse();
    let forms = shared.clone().resolve().unwrap();
    let reversed = reversed.resolve().unwrap();
    let mut left = pool.begin().await.unwrap();
    let mut right = pool.begin().await.unwrap();
    let left_verified =
        preflight_identities(&mut left, entries[0], &forms, IdentityCheck::Draft, &[])
            .await
            .unwrap();
    let right_verified =
        preflight_identities(&mut right, entries[1], &reversed, IdentityCheck::Draft, &[])
            .await
            .unwrap();
    // Each transaction has claimed its first content before either tries the other's ID.
    for (tx, entry, content) in [
        (&mut left, entries[0], shared.contents[0].id),
        (&mut right, entries[1], shared.contents[1].id),
    ] {
        sqlx::query("INSERT INTO lexicon.v3_form_contents(id,entry_id,content) VALUES($1,$2,'{}')")
            .bind(content)
            .bind(entry)
            .execute(&mut **tx)
            .await
            .unwrap();
    }
    let (left, right) = tokio::join!(
        async {
            let result = persist(&mut left, entries[0], &forms, left_verified).await;
            left.rollback().await.unwrap();
            result
        },
        async {
            let result = persist(&mut right, entries[1], &reversed, right_verified).await;
            right.rollback().await.unwrap();
            result
        }
    );
    assert!(
        matches!(
            (&left, &right),
            (Err(LexiconServiceError::StableNodeIdChanged), Ok(()))
                | (Ok(()), Err(LexiconServiceError::StableNodeIdChanged))
        ),
        "left={left:?}, right={right:?}"
    );
    let partial: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.v3_form_contents WHERE entry_id=ANY($1)")
            .bind(&entries[..])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(partial, 0);
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
