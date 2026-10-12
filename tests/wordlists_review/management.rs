use super::*;

#[sqlx::test]
async fn management_platform_create_edit_visibility_recommend_delete(pool: PgPool) {
    let (state, user) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let a = full_entry(&pool, "apple").await;
    let b = entry(&pool, "banana").await;
    let body = json!({"idempotency_key":Uuid::now_v7(),"name":"平台词表","items":[{"entry_id":a,"admin_note":"后台机密"},{"entry_id":b,"admin_note":""}]});
    let created = admin_call(
        &state,
        manager,
        "POST",
        "/api/v1/admin/wordlists",
        body.clone(),
    )
    .await;
    assert_eq!(created.0, StatusCode::OK, "{}", created.1);
    assert_eq!(
        created,
        admin_call(&state, manager, "POST", "/api/v1/admin/wordlists", body).await
    );
    assert!(created.1["wordlist"]["owner_user_id"].is_null());
    assert_eq!(created.1["review_state"], "none");
    let id = created.1["wordlist"]["id"].as_str().unwrap();
    let path = format!("/api/v1/admin/wordlists/{id}");
    sqlx::query(
        "INSERT INTO student_profiles(user_id,cefr_level,english_variant) VALUES($1,'A1','BrE')",
    )
    .bind(user.subject)
    .execute(&pool)
    .await
    .unwrap();
    let learning_body = json!({"task_type":"longterm","wordlist_ids":[id],"daily_question_count":null,"ends_at":null});
    let fuzzy = admin_call(
        &state,
        manager,
        "GET",
        "/api/v1/admin/wordlists/catalog?q=aple&page_size=3",
        Value::Null,
    )
    .await;
    assert_eq!(fuzzy.0, StatusCode::OK, "{}", fuzzy.1);
    assert_eq!(fuzzy.1["items"][0]["entry_id"], a.to_string());
    assert_eq!(
        call(
            &state,
            &user,
            "GET",
            &format!("/api/v1/wordlists/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let edited = admin_call(&state, manager, "GET", &format!("{path}/edit"), Value::Null).await;
    assert_eq!(edited.1["items"][0]["admin_note"], "后台机密");
    assert_eq!(edited.1["items"][0]["candidate"]["label"], "apple");
    assert_eq!(
        admin_call(
            &state,
            manager,
            "POST",
            &format!("{path}/recommendation"),
            json!({"expected_revision":1,"recommended":true})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let public = admin_call(
        &state,
        manager,
        "POST",
        &format!("{path}/visibility"),
        json!({"expected_revision":1,"public":true}),
    )
    .await;
    assert_eq!(public.0, StatusCode::OK, "{}", public.1);
    assert_eq!(public.1["review_state"], "none");
    let learning = call(
        &state,
        &user,
        "POST",
        "/api/v1/me/learning-tasks/preview",
        learning_body.clone(),
    )
    .await;
    assert_eq!(learning.0, StatusCode::OK, "{}", learning.1);
    assert!(learning.1["eligible_count"].as_i64().unwrap() > 0);
    let recommended = admin_call(
        &state,
        manager,
        "POST",
        &format!("{path}/recommendation"),
        json!({"expected_revision":2,"recommended":true}),
    )
    .await;
    assert_eq!(recommended.0, StatusCode::OK, "{}", recommended.1);
    let found = call(
        &state,
        &user,
        "GET",
        "/api/v1/wordlists/recommended",
        Value::Null,
    )
    .await;
    assert_eq!(found.1["items"][0]["id"], id);
    let read = call(
        &state,
        &user,
        "GET",
        &format!("/api/v1/wordlists/{id}/items"),
        Value::Null,
    )
    .await;
    assert_eq!(read.0, StatusCode::OK, "{}", read.1);
    assert!(!read.1.to_string().contains("后台机密"));
    assert_eq!(
        call(
            &state,
            &user,
            "POST",
            &format!("/api/v1/wordlists/{id}/tips"),
            json!({"event_id":Uuid::now_v7(),"idempotency_key":Uuid::now_v7(),"amount":"1"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let filtered = admin_call(
        &state,
        manager,
        "GET",
        "/api/v1/admin/wordlists/management?q=Reviewer&public=true&recommended=true&page_size=1",
        Value::Null,
    )
    .await;
    assert_eq!(filtered.1["pagination"]["total"], 1, "{}", filtered.1);
    assert_eq!(filtered.1["items"][0]["tip_amount"], "0");
    assert_eq!(
        admin_call(
            &state,
            manager,
            "GET",
            "/api/v1/admin/wordlists/management?created_from=2099-01-01T00:00:00Z",
            Value::Null
        )
        .await
        .1["pagination"]["total"],
        0
    );
    let update = json!({"expected_revision":3,"name":"改名","items":[{"entry_id":b,"admin_note":"先"},{"entry_id":a,"admin_note":"后"}]});
    let saved = admin_call(&state, manager, "PUT", &path, update.clone()).await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    assert_eq!(saved.1["wordlist"]["state"], "draft");
    assert_eq!(saved.1["recommended"], false);
    assert_eq!(
        call(
            &state,
            &user,
            "POST",
            "/api/v1/me/learning-tasks/preview",
            learning_body
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        admin_call(&state, manager, "PUT", &path, update).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &state,
            &user,
            "GET",
            "/api/v1/wordlists/recommended",
            Value::Null
        )
        .await
        .1["pagination"]["total"],
        0
    );
    let reopened = admin_call(&state, manager, "GET", &format!("{path}/edit"), Value::Null).await;
    assert_eq!(reopened.1["items"][0]["entry_id"], b.to_string());
    assert_eq!(reopened.1["items"][0]["admin_note"], "先");
    assert_eq!(
        admin_call(
            &state,
            manager,
            "POST",
            &format!("{path}/delete"),
            json!({"expected_revision":4})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    for endpoint in [
        path.clone(),
        format!("{path}/edit"),
        format!("{path}/items"),
    ] {
        assert_eq!(
            admin_call(&state, manager, "GET", &endpoint, Value::Null)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        admin_call(
            &state,
            manager,
            "GET",
            "/api/v1/admin/wordlists/management",
            Value::Null
        )
        .await
        .1["pagination"]["total"],
        0
    );
}

#[sqlx::test]
async fn user_publication_override_preserves_review_and_edit_invalidates_snapshot(pool: PgPool) {
    let (state, user) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let a = entry(&pool, "pending").await;
    let created = call(
        &state,
        &user,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[a]),
    )
    .await;
    let id = created.1["id"].as_str().unwrap();
    let path = format!("/api/v1/admin/wordlists/{id}");
    let pending = call(
        &state,
        &user,
        "POST",
        &format!("/api/v1/me/wordlists/{id}/review-requests"),
        json!({"expected_revision":1,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    let request = pending.1["id"].as_str().unwrap();
    for (revision, public) in [(2, true), (3, false)] {
        let changed = admin_call(
            &state,
            manager,
            "POST",
            &format!("{path}/visibility"),
            json!({"expected_revision":revision,"public":public}),
        )
        .await;
        assert_eq!(changed.0, StatusCode::OK, "{}", changed.1);
        assert_eq!(changed.1["review_state"], "pending");
    }
    let edit = admin_call(&state, manager, "GET", &format!("{path}/edit"), Value::Null).await;
    assert!(!edit.1.to_string().contains("私密备注"));
    let updated = admin_call(
        &state,
        manager,
        "PUT",
        &path,
        json!({"expected_revision":4,"name":"管理员改名","items":[{"entry_id":a,"admin_note":""}]}),
    )
    .await;
    assert_eq!(updated.0, StatusCode::OK, "{}", updated.1);
    assert_eq!(
        updated.1["wordlist"]["owner_user_id"],
        user.subject.to_string()
    );
    assert_eq!(
        admin_call(
            &state,
            manager,
            "POST",
            &format!("{path}/review-requests/{request}/decision"),
            json!({"expected_revision":2,"approve":true,"reason":null})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mine = call(
        &state,
        &user,
        "GET",
        &format!("/api/v1/me/wordlists/{id}/items"),
        Value::Null,
    )
    .await;
    assert_eq!(mine.1["items"][0]["private_note"], "私密备注");
    let body = json!({"expected_revision":5,"idempotency_key":Uuid::now_v7()});
    let published = call(
        &state,
        &user,
        "POST",
        &format!("/api/v1/me/wordlists/{id}/publish"),
        body.clone(),
    )
    .await;
    assert_eq!(published.0, StatusCode::OK, "{}", published.1);
    assert_eq!(published.1["state"], "approved");
    assert_eq!(
        published,
        call(
            &state,
            &user,
            "POST",
            &format!("/api/v1/me/wordlists/{id}/publish"),
            body
        )
        .await
    );
    assert_eq!(
        call(
            &state,
            &user,
            "GET",
            &format!("/api/v1/wordlists/{id}"),
            Value::Null
        )
        .await
        .1["state"],
        "published"
    );
    let hidden = admin_call(
        &state,
        manager,
        "POST",
        &format!("{path}/visibility"),
        json!({"expected_revision":6,"public":false}),
    )
    .await;
    assert_eq!(hidden.1["review_state"], "approved");
}

#[sqlx::test]
async fn management_permissions_and_migration_rollback_guard(pool: PgPool) {
    // Empty migration rollback and reapply preserve the prior schema.
    sqlx::raw_sql(include_str!(
        "../../migrations/20261012120000_wordlist_management.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    // A legacy draft may retain an approval for older content. Do not migrate that
    // historical decision into an approval of the current draft.
    let (_, legacy_user) = setup_bound(&pool).await;
    let legacy_list = Uuid::now_v7();
    sqlx::query("INSERT INTO wordlists(id,owner_user_id,name,state,revision,create_key,create_hash) VALUES($1,$2,'legacy edited draft','draft',4,$3,'hash')").bind(legacy_list).bind(legacy_user.subject).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO wordlist_review_requests(id,wordlist_id,submitted_revision,name,entry_ids,state,submit_key,request_hash) VALUES($1,$2,2,'old content',ARRAY[$3]::uuid[],'approved',$4,'hash')").bind(Uuid::now_v7()).bind(legacy_list).bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261012120000_wordlist_management.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let migrated: String = sqlx::query_scalar("SELECT review_state FROM wordlists WHERE id=$1")
        .bind(legacy_list)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(migrated, "none");
    let (state, _) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let a = entry(&pool, "permissions").await;
    let created=admin_call(&state,manager,"POST","/api/v1/admin/wordlists",json!({"idempotency_key":Uuid::now_v7(),"name":"权限检查","items":[{"entry_id":a,"admin_note":""}]})).await;
    assert_eq!(created.0, StatusCode::OK, "{}", created.1);
    let id = created.1["wordlist"]["id"].as_str().unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../../migrations/20261012120000_wordlist_management.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
    let viewer = admin(&pool).await;
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(viewer)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'wordlists.access',$2)").bind(viewer).bind(manager).execute(&pool).await.unwrap();
    assert_eq!(
        admin_call(
            &state,
            viewer,
            "GET",
            "/api/v1/admin/wordlists/management",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    for (method, path, body) in [
        (
            "POST",
            "/api/v1/admin/wordlists".to_string(),
            json!({"idempotency_key":Uuid::now_v7(),"name":"越权","items":[{"entry_id":a,"admin_note":""}]}),
        ),
        (
            "GET",
            format!("/api/v1/admin/wordlists/{id}/edit"),
            Value::Null,
        ),
        (
            "PUT",
            format!("/api/v1/admin/wordlists/{id}"),
            json!({"expected_revision":1,"name":"越权","items":[{"entry_id":a,"admin_note":""}]}),
        ),
        (
            "POST",
            format!("/api/v1/admin/wordlists/{id}/visibility"),
            json!({"expected_revision":1,"public":true}),
        ),
        (
            "POST",
            format!("/api/v1/admin/wordlists/{id}/recommendation"),
            json!({"expected_revision":1,"recommended":true}),
        ),
        (
            "POST",
            format!("/api/v1/admin/wordlists/{id}/delete"),
            json!({"expected_revision":1}),
        ),
    ] {
        assert_eq!(
            admin_call(&state, viewer, method, &path, body).await.0,
            StatusCode::FORBIDDEN,
            "{method} {path}"
        );
    }
}
