use super::*;
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;
use tsz_rust::{
    auth::extract::AuthUser,
    platform::storage::{
        MemoryAdapter, StoragePolicy, StoragePrivacy, StorageRegistry, StorageSpace,
    },
    state::AppState,
};
use uuid::Uuid;

fn audio_store(state: &mut AppState) {
    state.object_storage = StorageRegistry::from_stores([MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(
            StoragePrivacy::Private,
            1024,
            std::time::Duration::from_secs(60),
            None,
        )
        .unwrap(),
    )])
    .unwrap();
}

async fn create_list(state: &AppState, auth: &AuthUser, words: &[&str]) -> (Uuid, Vec<Uuid>) {
    let ids: Vec<_> = words.iter().map(|_| Uuid::now_v7()).collect();
    let items: Vec<_> = ids.iter().zip(words).map(|(id, word)| json!({
        "item_id":id,"custom_word":{"word":word,"definition":"测试释义"},"private_note":"PRIVATE NOTE"
    })).collect();
    let result = call(
        state,
        auth,
        "POST",
        "/api/v1/me/wordlists",
        json!({
            "idempotency_key":Uuid::now_v7(),"name":"语音读取测试","items":items
        }),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    (
        Uuid::parse_str(result.1["id"].as_str().unwrap()).unwrap(),
        ids,
    )
}

// Read-only fixtures bypass synthesis and payment, but keep real foreign keys
// and object-attempt relationships so handler authorization is exercised.
async fn seed_job(
    pool: &PgPool,
    owner: Uuid,
    list: Uuid,
    item: Uuid,
    word: &str,
    status: &str,
) -> (Uuid, Uuid) {
    let operation = Uuid::now_v7();
    sqlx::query("INSERT INTO coin_operations(id,kind,actor_type,actor_id,idempotency_scope,idempotency_key,request_hash,source_type,source_id,reason) VALUES($1,'debit','user',$2,'audio-read-test',$3,$4,'audio-read-test',$3,'PRIVATE LEDGER')")
        .bind(operation).bind(owner).bind(operation.to_string()).bind(vec![0_u8;32]).execute(pool).await.unwrap();
    let job = Uuid::now_v7();
    let token = Uuid::now_v7();
    let voices = json!([
        {"provider_voice_id":"en-GB-SoniaNeural","locale":"en-GB","provider_version":"test"},
        {"provider_voice_id":"en-US-AriaNeural","locale":"en-US","provider_version":"test"}
    ]);
    sqlx::query("INSERT INTO wordlist_audio_jobs(id,wordlist_id,item_id,user_id,word,voices,state,charge_operation_id,refund_operation_id,lease_token) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
        .bind(job).bind(list).bind(item).bind(owner).bind(word).bind(voices).bind(status).bind(operation)
        .bind((status == "refunded").then_some(operation)).bind(token).execute(pool).await.unwrap();
    for locale in ["en-GB", "en-US"] {
        sqlx::query("INSERT INTO wordlist_audio_objects(job_id,attempt_token,locale,object_key,uploaded,cleanup_after) VALUES($1,$2,$3,$4,true,clock_timestamp()+interval '1 hour')")
            .bind(job).bind(token).bind(locale).bind(format!("wordlist-custom/{job}/{token}/{locale}.mp3")).execute(pool).await.unwrap();
    }
    (job, token)
}

fn query(ids: &[Uuid]) -> String {
    format!(
        "?item_ids={}",
        ids.iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
}

async fn public_get(state: &AppState, path: &str) -> (StatusCode, Value) {
    let response = tsz_rust::router(state.clone())
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    if status == StatusCode::OK {
        assert_eq!(response.headers()["cache-control"], "private, no-store");
    }
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[sqlx::test]
async fn statuses_enforce_owner_public_admin_and_batch_boundaries(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let (_, stranger) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let words = ["queued", "processing", "ready", "refunding", "refunded"];
    let (list, ids) = create_list(&state, &auth, &words).await;
    let mut expected = Vec::new();
    for (item, status) in ids.iter().zip(words) {
        let (job, _) = seed_job(&pool, auth.subject, list, *item, status, status).await;
        expected.push(
            json!({"item_id":item,"job_id":job,"word":status,"status":status,
            "locales":if status=="ready" {vec!["en-GB","en-US"]}else{vec![]}}),
        );
    }
    let owner = format!("/api/v1/me/wordlists/{list}/custom-audio");
    let public = format!("/api/v1/wordlists/{list}/custom-audio");
    let administrative = format!("/api/v1/admin/wordlists/{list}/custom-audio");
    let suffix = query(&ids);
    let result = call(
        &state,
        &auth,
        "GET",
        &format!("{owner}{suffix}"),
        Value::Null,
    )
    .await;
    assert_eq!(result, (StatusCode::OK, json!({"items":expected})));
    assert_eq!(
        call(
            &state,
            &stranger,
            "GET",
            &format!("{owner}{suffix}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        public_get(&state, &format!("{public}{suffix}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        admin_call(
            &state,
            manager,
            "GET",
            &format!("{administrative}{suffix}"),
            Value::Null
        )
        .await,
        result
    );

    let reader = admin(&pool).await;
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(reader)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        admin_call(
            &state,
            reader,
            "GET",
            &format!("{administrative}{suffix}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'wordlists.access',$2)")
        .bind(reader).bind(manager).execute(&pool).await.unwrap();
    assert_eq!(
        admin_call(
            &state,
            reader,
            "GET",
            &format!("{administrative}{suffix}"),
            Value::Null
        )
        .await,
        result
    );

    sqlx::query("UPDATE wordlists SET state='published',review_state='approved' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let visible = public_get(&state, &format!("{public}{suffix}")).await;
    assert_eq!(visible, (StatusCode::OK, json!({"items":[expected[2]]})));
    for bad in [
        "".to_owned(),
        "?item_ids=".into(),
        "?item_ids=bad".into(),
        format!("{}&object_key=private", query(&ids)),
        query(&[ids[0]; 101]),
    ] {
        assert_eq!(
            call(&state, &auth, "GET", &format!("{owner}{bad}"), Value::Null)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!("{owner}{}", query(&[ids[0]; 100])),
            Value::Null
        )
        .await
        .1,
        json!({"items":[expected[0]]})
    );
    assert_eq!(
        public_get(&state, &format!("{public}{}", query(&[Uuid::now_v7()])))
            .await
            .1,
        json!({"items":[]})
    );
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        public_get(&state, &format!("{public}{suffix}")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn signatures_require_current_word_ready_job_and_uploaded_current_attempt(pool: PgPool) {
    let (mut state, auth) = setup_bound(&pool).await;
    audio_store(&mut state);
    let (_, stranger) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let (list, ids) = create_list(&state, &auth, &["sound"]).await;
    let item = ids[0];
    let (job, token) = seed_job(&pool, auth.subject, list, item, "sound", "ready").await;
    let owner = format!("/api/v1/me/wordlists/{list}/custom-audio/{item}");
    let public = format!("/api/v1/wordlists/{list}/custom-audio/{item}");
    for locale in ["en-GB", "en-US"] {
        let signed = call(
            &state,
            &auth,
            "GET",
            &format!("{owner}/{locale}"),
            Value::Null,
        )
        .await;
        assert_eq!(signed.0, StatusCode::OK, "{}", signed.1);
        assert_eq!(signed.1["url_expires_in_seconds"], 60);
        assert_eq!(signed.1.as_object().unwrap().len(), 3);
        assert!(
            signed.1["url"]
                .as_str()
                .unwrap()
                .contains(&format!("/{job}/{token}/{locale}.mp3"))
        );
        assert_eq!(
            call(
                &state,
                &stranger,
                "GET",
                &format!("{owner}/{locale}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            public_get(&state, &format!("{public}/{locale}")).await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        call(&state, &auth, "GET", &format!("{owner}/fr-FR"), Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!("/api/v1/me/wordlists/{list}/custom-audio/object-key/en-GB"),
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("UPDATE wordlists SET state='published',review_state='approved' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        public_get(&state, &format!("{public}/en-GB")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        admin_call(
            &state,
            manager,
            "GET",
            &format!("/api/v1/admin/wordlists/{list}/custom-audio/{item}/en-GB"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );

    let stale = Uuid::now_v7();
    sqlx::query("INSERT INTO wordlist_audio_objects(job_id,attempt_token,locale,object_key,uploaded,cleanup_after) VALUES($1,$2,'en-GB',$3,true,clock_timestamp())")
        .bind(job).bind(stale).bind(format!("stale/{stale}.mp3")).execute(&pool).await.unwrap();
    sqlx::query(
        "UPDATE wordlist_audio_objects SET uploaded=false WHERE job_id=$1 AND attempt_token=$2",
    )
    .bind(job)
    .bind(token)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        call(&state, &auth, "GET", &format!("{owner}/en-GB"), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE wordlist_audio_objects SET uploaded=true,deleted_at=clock_timestamp() WHERE job_id=$1 AND attempt_token=$2")
        .bind(job).bind(token).execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &auth, "GET", &format!("{owner}/en-GB"), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE wordlist_audio_objects SET deleted_at=NULL WHERE job_id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE wordlist_audio_jobs SET state='processing' WHERE id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        public_get(&state, &format!("{public}/en-GB")).await.0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE wordlist_audio_jobs SET state='ready',is_current=false WHERE id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &auth, "GET", &format!("{owner}/en-GB"), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE wordlist_audio_jobs SET is_current=true WHERE id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE wordlist_items SET custom_word=jsonb_set(custom_word,'{word}','\"changed\"') WHERE wordlist_id=$1 AND item_id=$2")
        .bind(list).bind(item).execute(&pool).await.unwrap();
    assert_eq!(
        public_get(
            &state,
            &format!("/api/v1/wordlists/{list}/custom-audio{}", query(&ids))
        )
        .await
        .1,
        json!({"items":[]})
    );
    assert_eq!(
        call(&state, &auth, "GET", &format!("{owner}/en-GB"), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE wordlist_items SET custom_word=jsonb_set(custom_word,'{word}','\" sound \"') WHERE wordlist_id=$1 AND item_id=$2")
        .bind(list).bind(item).execute(&pool).await.unwrap();
    assert_eq!(
        public_get(&state, &format!("{public}/en-GB")).await.0,
        StatusCode::OK
    );
    sqlx::query("UPDATE wordlist_audio_jobs SET test_run='unconfigured-read-test' WHERE id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &auth, "GET", &format!("{owner}/en-GB"), Value::Null)
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        public_get(&state, &format!("{public}/en-GB")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn review_audio_keeps_fixed_job_and_checks_snapshot_list_and_permission(pool: PgPool) {
    let (mut state, auth) = setup_bound(&pool).await;
    audio_store(&mut state);
    let manager = admin(&pool).await;
    let reader = admin(&pool).await;
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(reader)
        .execute(&pool)
        .await
        .unwrap();
    let (list, ids) = create_list(&state, &auth, &["submitted"]).await;
    let item = ids[0];
    let (old_job, old_token) =
        seed_job(&pool, auth.subject, list, item, "submitted", "ready").await;
    let submission = call(
        &state,
        &auth,
        "POST",
        &format!("/api/v1/me/wordlists/{list}/review-requests"),
        json!({
            "expected_revision":1,"idempotency_key":Uuid::now_v7()
        }),
    )
    .await;
    assert_eq!(submission.0, StatusCode::OK, "{}", submission.1);
    let request = Uuid::parse_str(submission.1["id"].as_str().unwrap()).unwrap();
    let review = format!("/api/v1/admin/wordlists/{list}/review-requests/{request}/custom-audio");
    let suffix = query(&ids);
    let statuses = admin_call(
        &state,
        manager,
        "GET",
        &format!("{review}{suffix}"),
        Value::Null,
    )
    .await;
    assert_eq!(statuses.0, StatusCode::OK, "{}", statuses.1);
    assert_eq!(statuses.1["items"][0]["job_id"], old_job.to_string());
    for path in [
        format!("{review}{suffix}"),
        format!("{review}/{item}/en-GB"),
    ] {
        assert_eq!(
            admin_call(&state, reader, "GET", &path, Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&state, &auth, "GET", &path, Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'wordlists.access',$2)")
        .bind(reader).bind(manager).execute(&pool).await.unwrap();
    sqlx::query("UPDATE wordlist_audio_jobs SET is_current=false WHERE id=$1")
        .bind(old_job)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE wordlist_items SET custom_word=jsonb_set(custom_word,'{word}','\"current\"') WHERE wordlist_id=$1")
        .bind(list).execute(&pool).await.unwrap();
    let (current_job, _) = seed_job(&pool, auth.subject, list, item, "current", "ready").await;
    assert_eq!(
        admin_call(
            &state,
            reader,
            "GET",
            &format!("{review}{suffix}"),
            Value::Null
        )
        .await,
        statuses
    );
    let signed = admin_call(
        &state,
        reader,
        "GET",
        &format!("{review}/{item}/en-GB"),
        Value::Null,
    )
    .await;
    assert_eq!(signed.0, StatusCode::OK, "{}", signed.1);
    assert!(
        signed.1["url"]
            .as_str()
            .unwrap()
            .contains(&format!("/{old_job}/{old_token}/"))
    );
    let current = admin_call(
        &state,
        manager,
        "GET",
        &format!("/api/v1/admin/wordlists/{list}/custom-audio{suffix}"),
        Value::Null,
    )
    .await;
    assert_eq!(current.1["items"][0]["job_id"], current_job.to_string());
    let (other_list, _) = create_list(&state, &auth, &["unrelated"]).await;
    for prefix in [
        format!("/api/v1/admin/wordlists/{other_list}/review-requests/{request}/custom-audio"),
        format!(
            "/api/v1/admin/wordlists/{list}/review-requests/{}/custom-audio",
            Uuid::now_v7()
        ),
    ] {
        for path in [
            format!("{prefix}{suffix}"),
            format!("{prefix}/{item}/en-GB"),
        ] {
            assert_eq!(
                admin_call(&state, manager, "GET", &path, Value::Null)
                    .await
                    .0,
                StatusCode::NOT_FOUND
            );
        }
    }
    sqlx::query("UPDATE wordlist_audio_review_refs SET job_id=$2 WHERE review_id=$1")
        .bind(request)
        .bind(current_job)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        admin_call(
            &state,
            manager,
            "GET",
            &format!("{review}{suffix}"),
            Value::Null
        )
        .await
        .1,
        json!({"items":[]})
    );
    assert_eq!(
        admin_call(
            &state,
            manager,
            "GET",
            &format!("{review}/{item}/en-GB"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE wordlist_audio_review_refs SET job_id=$2 WHERE review_id=$1")
        .bind(request)
        .bind(old_job)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1")
        .bind(reader)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        admin_call(
            &state,
            reader,
            "GET",
            &format!("{review}/{item}/en-GB"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE wordlists SET deleted_at=clock_timestamp() WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        admin_call(
            &state,
            manager,
            "GET",
            &format!("{review}{suffix}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        admin_call(
            &state,
            manager,
            "GET",
            &format!("{review}/{item}/en-GB"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[test]
fn custom_audio_openapi_contains_routes_states_and_strict_shapes() {
    use utoipa::OpenApi;
    let document = serde_json::to_value(tsz_rust::openapi::ApiDoc::openapi()).unwrap();
    for prefix in [
        "/api/v1/me/wordlists/{id}/custom-audio",
        "/api/v1/wordlists/{id}/custom-audio",
        "/api/v1/admin/wordlists/{id}/custom-audio",
        "/api/v1/admin/wordlists/{id}/review-requests/{request_id}/custom-audio",
    ] {
        assert_eq!(
            document["paths"][prefix]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
                ["$ref"],
            "#/components/schemas/AudioStatuses"
        );
        let signed = format!("{prefix}/{{item_id}}/{{locale}}");
        assert_eq!(
            document["paths"][&signed]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
                ["$ref"],
            "#/components/schemas/AudioAssetUrlResponse"
        );
    }
    let schemas = &document["components"]["schemas"];
    for name in ["AudioGeneration", "AudioStatuses", "AudioStatus"] {
        assert_eq!(schemas[name]["additionalProperties"], false, "{name}");
    }
    assert_eq!(
        schemas["AudioState"]["enum"],
        json!(["queued", "processing", "ready", "refunding", "refunded"])
    );
    let properties = schemas["AudioStatus"]["properties"].as_object().unwrap();
    assert_eq!(properties.len(), 5);
    for field in ["item_id", "job_id", "word", "status", "locales"] {
        assert!(properties.contains_key(field));
    }
}
