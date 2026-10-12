use super::*;
use std::time::Instant;

#[sqlx::test]
async fn batch5_duplicate_creation_only_requires_annotations(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let first = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let mut input = entry_annotations_create_body(
        &state,
        &bearer,
        "harbour",
        json!({"mode":"unified","common":"harbour"}),
    )
    .await;
    let key = Uuid::now_v7();
    let (status, conflict) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["code"], "annotation_conflict");
    input["annotation"] = json!("2");
    input["annotation_updates"] = entry_annotations_updates(&conflict, &["1"]);
    let (status, created) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_ne!(created["word"]["id"], first["word"]["id"]);
    let metadata: Value = sqlx::query_scalar("SELECT metadata FROM audit.admin_actions WHERE action='lexicon.entry.create.v3' AND resource_id=$1")
        .bind(Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
    assert!(metadata.get("homograph_reason").is_none(), "{metadata}");
    let (status, replay) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay, created);
    input["annotation"] = json!("3");
    let (status, changed) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CONFLICT, "{changed}");
    assert_eq!(changed["code"], "idempotency_conflict");
    input["homograph_reason"] = json!("obsolete field");
    let (status, rejected) =
        entry_annotations_submit(&state, &bearer, Uuid::now_v7(), &mut input).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2, "已删除的字段不能进入创建流程");
}

#[sqlx::test]
async fn batch5_list_references_and_maximum_batch_measurement(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let ((), router_stats) = with_test_router(&state, async {
        let actor = seed_admin(&pool).await;
        let bearer = token(&state, actor);
        let fixture_started = Instant::now();
        let surfaces = (0..50)
            .map(|index| format!("sample{index}"))
            .collect::<Vec<_>>();
        let prototype = create_legacy_v3_empty_skeleton(&state, &bearer, &surfaces[0]).await;
        let mut ids = vec![prototype];
        ids.extend(
            fixtures::seed_empty_v3_entries(&pool, prototype, &surfaces[0], &surfaces[1..]).await,
        );
        record_test_phase("maximum-batch", "empty-fixture", fixture_started);
        let content_started = Instant::now();
        let mut words = stream::iter(ids.iter().zip(&surfaces))
            .map(|(id, surface)| {
                let state = &state;
                let bearer = &bearer;
                async move {
                    let (_, forms) = save_v3_forms_after_impact(
                        state,
                        bearer,
                        &id.to_string(),
                        1,
                        "complete",
                        v3_forms_fixture_for(surface),
                    )
                    .await;
                    let meanings = complete_v3_meanings_fixture(
                        forms["word"]["forms"]["pos"][0]["pos_id"].clone(),
                    );
                    save_v3_meanings(state, bearer, &forms, meanings).await
                }
            })
            .buffered(4)
            .collect::<Vec<_>>()
            .await;
        record_test_phase("maximum-batch", "content-fixture", content_started);
        let relations_started = Instant::now();
        // 一个显式选择的环：每条依赖下一条，批次达到 API 的 50 条上限。
        for index in 0..words.len() {
            let target = &words[(index + 1) % words.len()];
            let mut meanings = writable_v3_meanings(&words[index]);
            meanings["pos"][0]["senses"][0]["relations"] = json!([{
                "id":Uuid::now_v7(), "relation":"synonym", "score":"80.00",
                "target_word_id":target["word"]["id"],
                "target_sense_id":target["word"]["meanings"]["pos"][0]["senses"][0]["id"]
            }]);
            words[index] = save_v3_meanings(&state, &bearer, &words[index], meanings).await;
        }
        record_test_phase("maximum-batch", "relation-fixture", relations_started);
        let start = Instant::now();
        let (status, published) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries/publications/batch"),
            &bearer,
            Some(Uuid::now_v7()),
            Some(batch3_input(&words.iter().collect::<Vec<_>>())),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{published}");
        assert_eq!(published["words"].as_array().unwrap().len(), 50);
        record_test_phase("maximum-batch", "publish-assertions", start);
        eprintln!(
            "batch5 measurement: entries=50 cyclic_relations=50 batch_ms={:.2}",
            start.elapsed().as_secs_f64() * 1000.0
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publications")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 50);
        let reference_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM lexicon.entry_publication_sense_refs reference
         JOIN lexicon.entries source ON source.id=reference.entry_id
            AND source.current_publication_id=reference.publication_id
         JOIN lexicon.entries target ON target.id=reference.target_entry_id
            AND target.current_publication_id=reference.target_publication_id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(reference_count, 50, "整个循环的发布引用必须持久化");
        let mut list_times = Vec::new();
        let mut reference_times = Vec::new();
        let reads_started = Instant::now();
        for iteration in 0..20 {
            let start = Instant::now();
            let (status, list) = call(
                &state,
                Method::GET,
                &format!("{ROOT}/entries?page={}&page_size=20", iteration % 3 + 1),
                &bearer,
                None,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{list}");
            assert_eq!(list["page"]["total"], 50);
            assert_eq!(
                list["words"].as_array().unwrap().len(),
                if iteration % 3 == 2 { 10 } else { 20 }
            );
            list_times.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = Instant::now();
            let word = &words[iteration % words.len()]["word"];
            let refs = inbound_references_of(&state, &bearer, word["id"].as_str().unwrap()).await;
            let (_, total) =
                node_total(&refs, &word["meanings"]["pos"][0]["senses"][0]["id"]).unwrap();
            assert!(total >= 1, "{refs}");
            reference_times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        record_test_phase("maximum-batch", "list-reference-assertions", reads_started);
        for (name, mut samples) in [("list", list_times), ("references", reference_times)] {
            samples.sort_by(f64::total_cmp);
            eprintln!(
                "batch5 measurement: {name} samples=20 p50_ms={:.2} p95_ms={:.2}",
                samples[9], samples[18]
            );
        }
    })
    .await;
    let expected_requests = BTreeMap::from([
        ("detection", 1),
        ("create", 1),
        ("forms-impact", 50),
        ("forms-save", 50),
        ("meanings-save", 100),
        ("publish-batch", 1),
        ("list", 20),
        ("references", 20),
    ]);
    assert_eq!(router_stats.router_builds, 1);
    assert_eq!(router_stats.requests, expected_requests);
    assert_eq!(router_stats.request_ids.len(), 243);
    eprintln!(
        "SQLX_CI_ROUTER_PILOT {}",
        json!({
            "test": concat!(module_path!(), "::", "batch5_list_references_and_maximum_batch_measurement"),
            "router_builds": router_stats.router_builds,
            "requests": router_stats.requests,
            "unique_request_ids": router_stats.request_ids.len(),
        })
    );
}

#[sqlx::test]
async fn scoped_router_preserves_alternate_state_and_restores_nested_scopes(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let disabled_read = state
        .clone()
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags {
            read: false,
            ..SmartLexiconV3Flags::all_enabled()
        });
    let uri = format!("{ROOT}/entries/{}/inbound-references", Uuid::now_v7());
    let ((), outer) = with_test_router(&state, async {
        let (status, _) = call(&state, Method::GET, &uri, &bearer, None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&disabled_read, Method::GET, &uri, &bearer, None, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let ((), inner) = with_test_router(&disabled_read, async {
            let (status, _) = call(&disabled_read, Method::GET, &uri, &bearer, None, None).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        })
        .await;
        assert_eq!(inner.router_builds, 1);
        assert_eq!(inner.request_ids.len(), 1);
        let (status, _) = call(&state, Method::GET, &uri, &bearer, None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    })
    .await;
    assert_eq!(outer.router_builds, 2);
    assert_eq!(outer.request_ids.len(), 3);
    assert!(TEST_ROUTER.try_with(|_| ()).is_err());
}

#[sqlx::test]
async fn scoped_router_keeps_swagger_and_rechecks_authorization_per_request(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let uri = format!("{ROOT}/entries/{}/inbound-references", Uuid::now_v7());
    let ((), stats) = with_test_router(&state, async {
        #[cfg(feature = "swagger")]
        {
            let (status, spec) = call(
                &state,
                Method::GET,
                "/api-docs/openapi.json",
                &bearer,
                None,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                spec["paths"]["/api/v1/admin/lexicon/entries/{id}/steps/forms"]["put"].is_object()
            );
        }
        let (status, _) = call(&state, Method::GET, &uri, &bearer, None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        sqlx::query("UPDATE admins SET status='disabled' WHERE id=$1")
            .bind(actor)
            .execute(&pool)
            .await
            .unwrap();
        let (status, _) = call(&state, Method::GET, &uri, &bearer, None, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    })
    .await;
    assert_eq!(stats.router_builds, 1);
    assert_eq!(
        stats.request_ids.len(),
        if cfg!(feature = "swagger") { 3 } else { 2 }
    );
    assert!(TEST_ROUTER.try_with(|_| ()).is_err());
}

#[sqlx::test]
async fn batch5_legacy_headwords_check_every_materialized_prototype(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let bearer = token(&state, seed_admin(&pool).await);
    let existing = batch3_word(&state, &bearer, "practise").await;
    seed_dictionary_word(&pool, "practice").await;
    seed_dictionary_word(&pool, "practise").await;
    let dataset_id: i64 =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status = 'active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("UPDATE dictionary.terms SET pos=ARRAY['noun','verb'] WHERE dataset_id=$1 AND normalized_term='practice'")
        .bind(dataset_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE dictionary.terms SET pos=ARRAY['verb'] WHERE dataset_id=$1 AND normalized_term='practise'")
        .bind(dataset_id).execute(&pool).await.unwrap();
    sqlx::query(r#"
        INSERT INTO dictionary.content_imports
        (dataset_id,input_sha256,source_locator,source_version,record_count,parser_version)
        VALUES ($1,repeat('b',64),'https://kaikki.org/test-source','enwiktionary-content-test',3,'forms-sounds-v1')
    "#).bind(dataset_id).execute(&pool).await.unwrap();
    sqlx::query(r#"
        INSERT INTO dictionary.entry_contents
        (dataset_id,source_key,normalized_term,pos,senses,forms,sounds,source_locator)
        VALUES
        ($1,'batch5:practice:noun','practice','noun','[]'::jsonb,'[]'::jsonb,'[]'::jsonb,'https://kaikki.org/test-source'),
        ($1,'batch5:practice:verb','practice','verb','[]'::jsonb,
         '[{"form":"practise","tags":["alternative","UK"]}]'::jsonb,'[]'::jsonb,'https://kaikki.org/test-source'),
        ($1,'batch5:practise:verb','practise','verb','[]'::jsonb,'[]'::jsonb,'[]'::jsonb,'https://kaikki.org/test-source')
    "#).bind(dataset_id).execute(&pool).await.unwrap();
    let mut input = entry_annotations_create_body(
        &state,
        &bearer,
        "practice",
        json!({"mode":"unified","common":"practice"}),
    )
    .await;
    input.as_object_mut().unwrap().remove("headwords");
    let key = Uuid::now_v7();
    let (status, conflict) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["code"], "annotation_conflict");
    input["annotation"] = json!("2");
    input["annotation_updates"] = entry_annotations_updates(&conflict, &["1"]);
    let (status, created) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_ne!(created["word"]["id"], existing["word"]["id"]);
}
