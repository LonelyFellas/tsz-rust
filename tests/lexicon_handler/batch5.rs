use super::*;
use std::time::Instant;

#[sqlx::test]
async fn batch5_homograph_reason_is_required_atomic_and_audited(pool: PgPool) {
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
    input.as_object_mut().unwrap().remove("homograph_reason");
    let key = Uuid::now_v7();
    let (_, conflict) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    input["annotation"] = json!("2");
    input["annotation_updates"] = entry_annotations_updates(&conflict, &["1"]);
    let (status, rejected) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(rejected["field"], "homograph_reason");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let annotation: Option<String> =
        sqlx::query_scalar("SELECT annotation FROM lexicon.entries WHERE id=$1")
            .bind(Uuid::parse_str(first["word"]["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(annotation.is_none(), "拒绝不能改写原有词条标注");
    for invalid in ["   ".to_owned(), "x".repeat(501), "bad\nreason".to_owned()] {
        input["homograph_reason"] = json!(invalid);
        let (status, rejected) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    }
    input["homograph_reason"] = json!("  独立含义，不应合并到原词条  ");
    let (status, created) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_ne!(created["word"]["id"], first["word"]["id"]);
    let metadata: Value = sqlx::query_scalar("SELECT metadata FROM audit.admin_actions WHERE action='lexicon.entry.create.v3' AND resource_id=$1")
        .bind(Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(metadata["homograph_reason"], "独立含义，不应合并到原词条");
    let (status, replay) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay, created);
    input["homograph_reason"] = json!("不同理由");
    let (status, changed) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CONFLICT, "{changed}");
    assert_eq!(changed["code"], "idempotency_conflict");
}

#[sqlx::test]
async fn batch5_list_references_and_maximum_batch_measurement(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let mut words = Vec::new();
    for index in 0..50 {
        words.push(batch3_word(&state, &bearer, &format!("sample{index}")).await);
    }
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
    eprintln!(
        "batch5 measurement: entries=50 cyclic_relations=50 batch_ms={:.2}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publications")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 50);
    let mut list_times = Vec::new();
    let mut reference_times = Vec::new();
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
        let (_, total) = node_total(&refs, &word["meanings"]["pos"][0]["senses"][0]["id"]).unwrap();
        assert!(total >= 1, "{refs}");
        reference_times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    for (name, mut samples) in [("list", list_times), ("references", reference_times)] {
        samples.sort_by(f64::total_cmp);
        eprintln!(
            "batch5 measurement: {name} samples=20 p50_ms={:.2} p95_ms={:.2}",
            samples[9], samples[18]
        );
    }
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
    input.as_object_mut().unwrap().remove("homograph_reason");
    let key = Uuid::now_v7();
    let (status, conflict) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["code"], "annotation_conflict");
    input["annotation"] = json!("2");
    input["annotation_updates"] = entry_annotations_updates(&conflict, &["1"]);
    let (status, missing) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    assert_eq!(missing["field"], "homograph_reason");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let annotation: Option<String> =
        sqlx::query_scalar("SELECT annotation FROM lexicon.entries WHERE id=$1")
            .bind(Uuid::parse_str(existing["word"]["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(annotation.is_none());
    input["homograph_reason"] = json!("词性原型重合，但含义独立");
    let (status, created) = entry_annotations_submit(&state, &bearer, key, &mut input).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_ne!(created["word"]["id"], existing["word"]["id"]);
}
