mod account_deletion_support;
mod learning_tasks_support;
mod wordlists_support;
use account_deletion_support::call;
use learning_tasks_support::*;
use serde_json::Value;
use sqlx::PgPool;
#[sqlx::test]
async fn source_update_archive_and_deleted_source_receipt(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let shared_entry: uuid::Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    wordlists_support::share_current_publication(&pool, shared_entry).await;
    let task = create(&state, &auth, list, "longterm").await;
    let start_input = start_body(None);
    let start_path = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    let run = call(&state, &auth, "POST", &start_path, start_input.clone())
        .await
        .1;
    let qs = questions(&state, &auth, &run).await;
    let path = answer_path(&run);
    let first = answer_body(&qs["items"][0], "apple");
    sqlx::query("UPDATE wordlists SET name='rename',revision=revision+1 WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let entry: uuid::Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    let admin = wordlists_support::admin(&pool).await;
    let mut snapshot: Value = sqlx::query_scalar("SELECT p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1")
        .bind(entry).fetch_one(&pool).await.unwrap();
    snapshot["forms"]["contents"][0]["regional_variants"]["uk"]["spelling"] =
        serde_json::json!("new-answer");
    snapshot["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] =
        serde_json::json!("重发后的释义");
    let publication = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,3,3,3,$3,$4,$5)")
        .bind(publication).bind(entry).bind(snapshot).bind(publication.as_bytes().to_vec()).bind(admin).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=$2 WHERE id=$1")
        .bind(entry)
        .bind(publication)
        .execute(&pool)
        .await
        .unwrap();
    let restored = call(
        &state,
        &auth,
        "GET",
        &format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(restored.1["state"], "active");
    let restored_questions = questions(&state, &auth, &run).await;
    assert_eq!(restored_questions["items"], qs["items"]);
    let a = call(&state, &auth, "POST", &path, first.clone()).await;
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(a.1["is_correct"], true);
    assert_eq!(
        a.1["question"]["feedback"]["accepted_answers"],
        serde_json::json!(["apple", "alternate-base"])
    );
    let fresh_task = create(&state, &auth, list, "longterm").await;
    let fresh_run = start(&state, &auth, &fresh_task).await;
    let fresh_questions = questions(&state, &auth, &fresh_run).await;
    assert_eq!(
        fresh_questions["items"][0]["prompt"]["definition"],
        "重发后的释义"
    );
    let fresh_answer = call(
        &state,
        &auth,
        "POST",
        &answer_path(&fresh_run),
        answer_body(&fresh_questions["items"][0], "new-answer"),
    )
    .await;
    assert_eq!(fresh_answer.0, 200, "{}", fresh_answer.1);
    assert_eq!(fresh_answer.1["is_correct"], true);
    assert_eq!(
        fresh_answer.1["question"]["feedback"]["accepted_answers"],
        serde_json::json!(["new-answer", "alternate-base"])
    );
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE lexicon.entries SET archived_at=NULL WHERE id=$1")
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &path,
            answer_body(&qs["items"][1], "wrong")
        )
        .await
        .0,
        409
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_completions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("DELETE FROM wordlists WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let replay = call(&state, &auth, "POST", &path, first).await;
    assert_eq!(replay.0, 200, "{}", replay.1);
    assert_eq!(replay.1["answer_id"], a.1["answer_id"]);
    assert!(replay.1["question"]["feedback"].is_null());
    assert!(replay.1["question"]["prompt"].is_null());
    let replay = call(&state, &auth, "POST", &start_path, start_input).await;
    assert_eq!(replay.0, 200);
    assert_eq!(replay.1["id"], run["id"]);
}
#[sqlx::test]
async fn longterm_successor_concurrency_and_final_answer_rollback(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let qs = questions(&state, &auth, &run).await;
    let path = answer_path(&run);
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &path,
            answer_body(&qs["items"][0], "wrong")
        )
        .await
        .0,
        200
    );
    sqlx::raw_sql("CREATE FUNCTION fail_learning_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test completion failure'; END $$; CREATE TRIGGER fail_learning_completion BEFORE INSERT ON learning_completions FOR EACH ROW EXECUTE FUNCTION fail_learning_completion();").execute(&pool).await.unwrap();
    let last = answer_body(&qs["items"][1], "apple");
    assert_eq!(
        call(&state, &auth, "POST", &path, last.clone()).await.0,
        500
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_answers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    sqlx::query("DROP TRIGGER fail_learning_completion ON learning_completions")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(call(&state, &auth, "POST", &path, last).await.0, 200);
    let sp = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, &auth, "POST", &sp, start_body(None)).await.0,
        409
    );
    let body = start_body(run["id"].as_str());
    let other = start_body(run["id"].as_str());
    let (a, b) = tokio::join!(
        call(&state, &auth, "POST", &sp, body.clone()),
        call(&state, &auth, "POST", &sp, other)
    );
    assert!([a.0.as_u16(), b.0.as_u16()].contains(&200));
    assert!([a.0.as_u16(), b.0.as_u16()].contains(&409));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    let resumed = start(&state, &auth, &task).await;
    assert_ne!(resumed["id"], run["id"]);
}
#[sqlx::test]
async fn expiry_gate_and_old_keys_replay(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "daily").await;
    let input = start_body(None);
    let sp = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    let run = call(&state, &auth, "POST", &sp, input.clone()).await.1;
    let qs = questions(&state, &auth, &run).await;
    let first = answer_body(&qs["items"][0], "apple");
    let path = answer_path(&run);
    assert_eq!(
        call(&state, &auth, "POST", &path, first.clone()).await.0,
        200
    );
    sqlx::query("UPDATE learning_runs SET expires_at=clock_timestamp()-interval '1 second',business_day=business_day-1 WHERE id=$1").bind(uuid::Uuid::parse_str(run["id"].as_str().unwrap()).unwrap()).execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &auth, "POST", &sp, input).await.1["id"],
        run["id"]
    );
    let replay = call(&state, &auth, "POST", &path, first).await;
    assert_eq!(replay.0, 200);
    assert_eq!(replay.1["run"]["state"], "expired");
    assert!(replay.1["question"]["feedback"].is_object());
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &path,
            answer_body(&qs["items"][1], "apple")
        )
        .await
        .0,
        409
    );
    let fresh = start(&state, &auth, &task).await;
    assert_ne!(fresh["id"], run["id"]);
    let req = account_deletion_support::apply(&state, &auth, "0").await;
    account_deletion_support::deadline(&pool, req.id, -1).await;
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            "/api/v1/me/learning-tasks",
            Value::Null
        )
        .await
        .0,
        401
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn observed_invalidation_survives_failed_reopen(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let entry: uuid::Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let path = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, &auth, "POST", &path, start_body(None)).await.0,
        409
    );
    sqlx::query("INSERT INTO wordlist_items(wordlist_id,entry_id,position) VALUES($1,$2,0)")
        .bind(list)
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &auth, "POST", &path, start_body(None)).await.0,
        409
    );
    let next = call(&state, &auth, "POST", &path, start_body(run["id"].as_str())).await;
    assert_eq!(next.0, 200, "{}", next.1);
    assert_ne!(next.1["id"], run["id"]);
}
#[sqlx::test]
async fn start_write_crossing_deadline_rolls_back(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "daily").await;
    sqlx::query("UPDATE learning_tasks SET ends_at=clock_timestamp()+interval '0.7 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION slow_learning_start() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(1); RETURN NEW; END $$; CREATE TRIGGER slow_learning_start BEFORE INSERT ON learning_run_start_requests FOR EACH ROW EXECUTE FUNCTION slow_learning_start();").execute(&pool).await.unwrap();
    let path = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, &auth, "POST", &path, start_body(None)).await.0,
        409
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn source_owner_deletion_crossing_start_write_rolls_back(pool: PgPool) {
    let (state, owner, list) = setup(&pool).await;
    let (_, learner, _) = setup(&pool).await;
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let task = create(&state, &learner, list, "longterm").await;
    let request = account_deletion_support::apply(&state, &owner, "0").await;
    account_deletion_support::deadline(&pool, request.id, 700).await;
    sqlx::raw_sql("CREATE FUNCTION slow_learning_start() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(1); RETURN NEW; END $$; CREATE TRIGGER slow_learning_start BEFORE INSERT ON learning_run_start_requests FOR EACH ROW EXECUTE FUNCTION slow_learning_start();").execute(&pool).await.unwrap();
    let path = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, &learner, "POST", &path, start_body(None))
            .await
            .0,
        409
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn unrelated_large_append_preserves_run_and_large_pages_do_not_overflow(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let admin = wordlists_support::admin(&pool).await;
    let entries:Vec<uuid::Uuid>=sqlx::query_scalar("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,detection_snapshot,created_by_admin_id,updated_by_admin_id) SELECT gen_random_uuid(),3,'en','word','{}',$1,$1 FROM generate_series(1,1000) RETURNING id").bind(admin).fetch_all(&pool).await.unwrap();
    sqlx::query("INSERT INTO wordlist_items(wordlist_id,entry_id,position) SELECT $1,x.id,x.n::integer FROM unnest($2::uuid[]) WITH ORDINALITY AS x(id,n)").bind(list).bind(entries).execute(&pool).await.unwrap();
    let recovered = start(&state, &auth, &task).await;
    assert_eq!(recovered["id"], run["id"]);
    assert_eq!(recovered["state"], "active");
    let qs = questions(&state, &auth, &run).await;
    for q in qs["items"].as_array().unwrap() {
        assert_eq!(
            call(
                &state,
                &auth,
                "POST",
                &answer_path(&run),
                answer_body(q, "wrong")
            )
            .await
            .0,
            200
        );
    }
    for path in [
        "/api/v1/me/learning-tasks".to_owned(),
        format!(
            "/api/v1/me/learning-tasks/{}/runs",
            task["id"].as_str().unwrap()
        ),
        format!(
            "/api/v1/me/learning-runs/{}/questions",
            run["id"].as_str().unwrap()
        ),
    ] {
        let r = call(
            &state,
            &auth,
            "GET",
            &format!("{path}?page=4294967295&page_size=50"),
            Value::Null,
        )
        .await;
        assert_eq!(r.0, 200, "{}", r.1);
        assert_eq!(r.1["items"], serde_json::json!([]));
    }
}

#[sqlx::test]
async fn restored_membership_and_public_access_do_not_revive_old_runs(pool: PgPool) {
    let (state, owner, list) = setup(&pool).await;
    let task = create(&state, &owner, list, "longterm").await;
    let old = start(&state, &owner, &task).await;
    let entry: uuid::Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    let other = wordlists_support::entry(&pool, "unrelated").await;
    let edit = |revision, ids: Vec<uuid::Uuid>| serde_json::json!({"expected_revision":revision,"content":{"name":"source","entry_ids":ids},"note_updates":[]});
    let path = format!("/api/v1/me/wordlists/{list}");
    assert_eq!(
        call(&state, &owner, "PUT", &path, edit(1, vec![other]))
            .await
            .0,
        200
    );
    let run_path = format!("/api/v1/me/learning-runs/{}", old["id"].as_str().unwrap());
    // No run read between removal and reinsertion: the membership identity must
    // invalidate the old run even when nobody observed its temporary absence.
    assert_eq!(
        call(&state, &owner, "PUT", &path, edit(2, vec![entry, other]))
            .await
            .0,
        200
    );
    let restored = call(&state, &owner, "GET", &run_path, Value::Null).await.1;
    assert_eq!(
        restored["state"], "invalidated",
        "removed/reinserted member revived old run"
    );
    let (_, borrower, _) = setup(&pool).await;
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let task = create(&state, &borrower, list, "longterm").await;
    let old = start(&state, &borrower, &task).await;
    let own_task = create(&state, &owner, list, "longterm").await;
    let own = start(&state, &owner, &own_task).await;
    assert_eq!(
        call(
            &state,
            &owner,
            "POST",
            &format!("{path}/withdraw"),
            serde_json::json!({"expected_revision":3})
        )
        .await
        .0,
        200
    );
    let review = call(
        &state,
        &owner,
        "POST",
        &format!("{path}/review-requests"),
        serde_json::json!({"expected_revision":4,"idempotency_key":uuid::Uuid::now_v7()}),
    )
    .await;
    assert_eq!(review.0, 200, "{}", review.1);
    let admin = wordlists_support::admin(&pool).await;
    let approved = wordlists_support::admin_call(
        &state,
        admin,
        "POST",
        &format!(
            "/api/v1/admin/wordlists/{list}/review-requests/{}/decision",
            review.1["id"].as_str().unwrap()
        ),
        serde_json::json!({"expected_revision":5,"approve":true,"reason":null}),
    )
    .await;
    assert_eq!(approved.0, 200, "{}", approved.1);
    let run_path = format!("/api/v1/me/learning-runs/{}", old["id"].as_str().unwrap());
    assert_eq!(
        call(&state, &borrower, "GET", &run_path, Value::Null)
            .await
            .1["state"],
        "invalidated"
    );
    let own_path = format!("/api/v1/me/learning-runs/{}", own["id"].as_str().unwrap());
    assert_eq!(
        call(&state, &owner, "GET", &own_path, Value::Null).await.1["state"],
        "active"
    );
}

#[sqlx::test]
async fn source_owner_expiring_while_read_waits_hides_questions_and_receipts(pool: PgPool) {
    let (state, owner, list) = setup(&pool).await;
    let (_, learner, _) = setup(&pool).await;
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let task = create(&state, &learner, list, "longterm").await;
    let run = start(&state, &learner, &task).await;
    let qs = questions(&state, &learner, &run).await;
    let first = answer_body(&qs["items"][0], "apple");
    assert_eq!(
        call(&state, &learner, "POST", &answer_path(&run), first.clone())
            .await
            .0,
        200
    );
    let request = account_deletion_support::apply(&state, &owner, "0").await;
    let entry: uuid::Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    for receipt in [false, true] {
        account_deletion_support::deadline(&pool, request.id, 1500).await;
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM lexicon.entries WHERE id=$1 FOR UPDATE")
            .bind(entry)
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        let s = state.clone();
        let auth = tsz_rust::auth::extract::AuthUser {
            subject: learner.subject,
            role: "student".into(),
            security_version: learner.security_version,
        };
        let p = if receipt {
            answer_path(&run)
        } else {
            format!(
                "/api/v1/me/learning-runs/{}/questions",
                run["id"].as_str().unwrap()
            )
        };
        let body = if receipt { first.clone() } else { Value::Null };
        let read = tokio::spawn(async move {
            call(&s, &auth, if receipt { "POST" } else { "GET" }, &p, body).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2),async{loop{let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT id,current_publication_id%')").fetch_one(&pool).await.unwrap();if waiting{break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}}).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
        blocker.commit().await.unwrap();
        let result = read.await.unwrap();
        assert_eq!(result.0, 200, "{}", result.1);
        let q = if receipt {
            &result.1["question"]
        } else {
            &result.1["items"][0]
        };
        assert_eq!(q["content_available"], false, "{q}");
        assert!(q["prompt"].is_null());
        assert!(q["feedback"].is_null());
    }
}

mod learning_plans;

#[sqlx::test]
async fn answer_audio_is_private_fixed_and_uses_question_publication(pool: PgPool) {
    use std::{sync::Arc, time::Duration};
    use tsz_rust::platform::storage::{
        MemoryAdapter, ObjectContentType, ObjectKey, PutOptions, StoragePolicy, StoragePrivacy,
        StorageRegistry, StorageSpace,
    };
    use uuid::Uuid;
    let (mut state, auth, list) = setup(&pool).await;
    // Empty migration round trip. Populated facts must later block down.
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261031120000_learning_audio.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261031120000_learning_audio.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let store = MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    state.object_storage = StorageRegistry::from_stores([Arc::clone(&store)]).unwrap();
    let (entry, publication, mut snapshot, actor): (Uuid, Uuid, Value, Uuid) = sqlx::query_as("SELECT e.id,p.id,p.snapshot,p.published_by_admin_id FROM wordlist_items i JOIN lexicon.entries e ON e.id=i.entry_id JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE i.wordlist_id=$1")
        .bind(list).fetch_one(&pool).await.unwrap();
    let mut assets = Vec::new();
    for index in 0..2 {
        let asset = Uuid::now_v7();
        let key = format!("fixtures/learning/{asset}.mp3");
        store
            .put(
                &ObjectKey::parse(&key).unwrap(),
                b"ID3".to_vec(),
                PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
            )
            .await
            .unwrap();
        let value: Value = sqlx::query_scalar("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id,created_at) VALUES($1,$2,$2,'audio/mpeg',3,'en-GB','female','fixture.mp3',$3,now()-interval '30 days') RETURNING jsonb_build_object('id',id,'content_type',content_type,'size_bytes',size_bytes,'locale',locale,'gender',gender,'original_name',original_name,'duration_ms',duration_ms,'created_at',created_at)")
            .bind(asset).bind(key).bind(actor).fetch_one(&pool).await.unwrap();
        if index == 0 {
            assets.push(value);
        } else {
            assets.insert(0, value);
        }
    }
    snapshot["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["pronunciations"][0]["audio_assets"] =
        serde_json::json!(assets);
    snapshot["forms"]["pos"][0]["forms"][1]["regional_variants"]["uk"]["pronunciations"][0]["audio_assets"] =
        serde_json::json!(assets);
    let base_form = Uuid::parse_str(
        snapshot["forms"]["pos"][0]["forms"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let alternate_form = Uuid::parse_str(
        snapshot["forms"]["pos"][0]["forms"][1]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let mut another_sense = snapshot["meanings"]["pos"][0]["senses"][0].clone();
    another_sense["id"] = serde_json::json!(Uuid::now_v7());
    another_sense["definitions"][0]["id"] = serde_json::json!(Uuid::now_v7());
    another_sense["definitions"][0]["content_id"] = serde_json::json!(Uuid::now_v7());
    snapshot["meanings"]["pos"][0]["senses"] =
        serde_json::json!([snapshot["meanings"]["pos"][0]["senses"][0], another_sense]);
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=$1")
        .bind(publication)
        .bind(&snapshot)
        .execute(&pool)
        .await
        .unwrap();
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let qs = questions(&state, &auth, &run).await;
    let first = &qs["items"][0];
    // Different senses may retain disjoint form links; their vocabulary occurrence is shared.
    for (question, form) in [
        (&qs["items"][0], base_form),
        (&qs["items"][1], alternate_form),
    ] {
        sqlx::query("UPDATE learning_questions SET form_ids=$2 WHERE id=$1")
            .bind(Uuid::parse_str(question["id"].as_str().unwrap()).unwrap())
            .bind(vec![form])
            .execute(&pool)
            .await
            .unwrap();
    }

    let path = format!(
        "/api/v1/me/learning-runs/{}/questions/{}/audio?accent=uk",
        run["id"].as_str().unwrap(),
        first["id"].as_str().unwrap()
    );
    let denied = call(&state, &auth, "GET", &path, Value::Null).await;
    assert_eq!(denied.0, 403, "{}", denied.1);
    assert!(!denied.1.to_string().contains("asset_id"));
    assert!(!denied.1.to_string().contains("apple"));
    let (_, stranger) = account_deletion_support::setup_bound(&pool).await;
    assert_eq!(
        call(&state, &stranger, "GET", &path, Value::Null).await.0,
        404
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &answer_path(&run),
            answer_body(first, "apple")
        )
        .await
        .0,
        200
    );
    let (a, b) = tokio::join!(
        call(&state, &auth, "GET", &path, Value::Null),
        call(&state, &auth, "GET", &path, Value::Null)
    );
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(b.0, 200, "{}", b.1);
    assert_eq!(a.1["audio"]["asset_id"], assets[0]["id"]);
    assert_eq!(a.1["audio"]["asset_id"], b.1["audio"]["asset_id"]);
    assert_eq!(a.1["occurrence"], 1);
    assert_eq!(a.1["audio"]["effective_rank"], 1);
    assert_eq!(a.1["policy_version"], "harmonic_v1");
    let facts: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_question_audio")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(facts, 1);
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261031120000_learning_audio.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    // A later publication does not replace the frozen question source.
    let newer = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,99,99,3,$3,$4,$5)")
        .bind(newer).bind(entry).bind(&snapshot).bind(newer.as_bytes().to_vec()).bind(actor).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=$2 WHERE id=$1")
        .bind(entry)
        .bind(newer)
        .execute(&pool)
        .await
        .unwrap();
    let replay = call(&state, &auth, "GET", &path, Value::Null).await;
    assert_eq!(replay.0, 200, "{}", replay.1);
    assert_eq!(replay.1["audio"]["publication_id"], publication.to_string());
    // A different sense of the same word has a new question ID, never a new first occurrence.
    let second = &qs["items"][1];
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &answer_path(&run),
            answer_body(second, "apple")
        )
        .await
        .0,
        200
    );
    let second_path = format!(
        "/api/v1/me/learning-runs/{}/questions/{}/audio?accent=uk",
        run["id"].as_str().unwrap(),
        second["id"].as_str().unwrap()
    );
    let selected = call(&state, &auth, "GET", &second_path, Value::Null).await;
    assert_eq!(selected.0, 200, "{}", selected.1);
    assert_eq!(selected.1["occurrence"], 2);
    let repeat = call(&state, &auth, "GET", &second_path, Value::Null).await;
    assert_eq!(
        repeat.1["audio"]["asset_id"],
        selected.1["audio"]["asset_id"]
    );
    // Facts, even without publication reference rows in this synthetic fixture, protect assets.
    tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &store)
        .await
        .unwrap();
    let retained: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon.audio_assets WHERE id=$1)")
            .bind(Uuid::parse_str(a.1["audio"]["asset_id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retained);
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=NULL WHERE id=$1")
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(call(&state, &auth, "GET", &path, Value::Null).await.0, 404);
    let history = questions(&state, &auth, &run).await;
    assert_eq!(history["items"][0]["answered"], true);
}
