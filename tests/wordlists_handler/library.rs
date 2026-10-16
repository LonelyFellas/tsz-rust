use super::*;
use tsz_rust::auth::extract::AuthUser;
use tsz_rust::state::AppState;
use uuid::Uuid;
async fn listing(state: &AppState, auth: &AuthUser, space: &str, query: &str) -> Value {
    let (status, body) = call(
        state,
        auth,
        "GET",
        &format!("/api/v1/me/wordlist-library/{space}{query}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}
async fn folder(
    state: &AppState,
    auth: &AuthUser,
    space: &str,
    parent: Value,
    name: &str,
) -> Value {
    let current = listing(state, auth, space, "").await;
    let (s, b) = call(
        state,
        auth,
        "POST",
        &format!("/api/v1/me/wordlist-library/{space}/folders"),
        json!({"expected_version":current["version"],"parent_id":parent,"name":name}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    let query = if parent.is_null() {
        String::new()
    } else {
        format!("?parent_id={}", parent.as_str().unwrap())
    };
    listing(state, auth, space, &query).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == name)
        .unwrap()["id"]
        .clone()
}
async fn op(
    state: &AppState,
    auth: &AuthUser,
    space: &str,
    action: &str,
    ids: Vec<Value>,
    target: Value,
) -> Value {
    let v = listing(state, auth, space, "").await;
    let input = json!({"expected_version":v["version"],"action":action,"ids":ids,"target_id":target,"original_location":action=="restore"});
    let (s, b) = call(
        state,
        auth,
        "POST",
        &format!("/api/v1/me/wordlist-library/{space}/operations"),
        input,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b
}
#[sqlx::test]
async fn library_tree_trash_batches_restore_and_permanent_delete(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let a = folder(&state, &auth, "owned", Value::Null, "英语").await;
    let b = folder(&state, &auth, "owned", a.clone(), "考试").await;
    let e = entry(&pool, "apple").await;
    let mut body = create_body(&[e]);
    body["folder_id"] = b.clone();
    let (status, w) = call(&state, &auth, "POST", "/api/v1/me/wordlists", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{w}");
    assert_eq!(
        call(&state, &auth, "POST", "/api/v1/me/wordlists", body)
            .await
            .1,
        w
    );
    let children = listing(
        &state,
        &auth,
        "owned",
        &format!("?parent_id={}", b.as_str().unwrap()),
    )
    .await;
    let file = children["items"][0]["id"].clone();
    // The legacy content editor must invalidate a directory rename snapshot too.
    let old_version = listing(&state, &auth, "owned", "").await["version"].clone();
    let changed = call(&state, &auth, "PUT", &format!("/api/v1/me/wordlists/{}", w["id"].as_str().unwrap()),
        json!({"expected_revision":1,"content":{"name":"编辑器改名","entry_ids":[e]},"note_updates":[]})).await;
    assert_eq!(changed.0, StatusCode::OK, "{}", changed.1);
    let stale = call(
        &state,
        &auth,
        "PUT",
        &format!(
            "/api/v1/me/wordlist-library/owned/nodes/{}",
            file.as_str().unwrap()
        ),
        json!({"expected_version":old_version,"name":"过期改名"}),
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT);

    let wlpath = format!("/api/v1/me/wordlists/{}", w["id"].as_str().unwrap());
    op(
        &state,
        &auth,
        "owned",
        "trash",
        vec![file.clone()],
        Value::Null,
    )
    .await;
    let impact = op(
        &state,
        &auth,
        "owned",
        "trash",
        vec![a.clone(), b.clone()],
        Value::Null,
    )
    .await;
    assert_eq!(impact["folder_count"], 2);
    assert_eq!(impact["wordlist_count"], 0);
    assert_eq!(
        listing(&state, &auth, "owned", "?trash=true").await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    op(
        &state,
        &auth,
        "owned",
        "restore",
        vec![a.clone()],
        Value::Null,
    )
    .await;
    assert_eq!(
        call(&state, &auth, "GET", &wlpath, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    op(
        &state,
        &auth,
        "owned",
        "trash",
        vec![a.clone()],
        Value::Null,
    )
    .await;
    op(
        &state,
        &auth,
        "owned",
        "purge",
        vec![a.clone()],
        Value::Null,
    )
    .await;
    let trash = listing(&state, &auth, "owned", "?trash=true").await;
    assert_eq!(trash["items"].as_array().unwrap().len(), 1);
    assert!(trash["items"][0]["parent_id"].is_null());
    op(
        &state,
        &auth,
        "owned",
        "restore",
        vec![file.clone()],
        Value::Null,
    )
    .await;
    let restored = call(&state, &auth, "GET", &wlpath, Value::Null).await;
    assert_eq!(restored.0, StatusCode::OK);
    assert_eq!(restored.1["state"], "draft");
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!("{wlpath}/items"),
            Value::Null
        )
        .await
        .1["items"][0]["private_note"],
        "私密备注"
    );
    op(
        &state,
        &auth,
        "owned",
        "trash",
        vec![file.clone()],
        Value::Null,
    )
    .await;
    op(&state, &auth, "owned", "purge", vec![file], Value::Null).await;
    assert!(
        listing(&state, &auth, "owned", "?trash=true").await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM wordlist_items WHERE wordlist_id=$1")
        .bind(w["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[sqlx::test]
async fn library_isolation_cycles_stale_confirmations_and_name_conflicts(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let (_, other) = setup_bound(&pool).await;
    let a = folder(&state, &auth, "owned", Value::Null, "英语").await;
    let b = folder(&state, &auth, "owned", a.clone(), "子目录").await;
    folder(&state, &auth, "favorites", Value::Null, "英语").await;
    let old = listing(&state, &auth, "owned", "").await;
    let make = |action: &str, target: Value| json!({"expected_version":old["version"],"action":action,"ids":[a],"target_id":target});
    let endpoint = "/api/v1/me/wordlist-library/owned/operations";
    assert_eq!(
        call(&state, &auth, "POST", endpoint, make("move", b))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &state,
            &other,
            "GET",
            &format!(
                "/api/v1/me/wordlist-library/owned?parent_id={}",
                a.as_str().unwrap()
            ),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!(
                "/api/v1/me/wordlist-library/favorites?parent_id={}",
                a.as_str().unwrap()
            ),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let create = json!({"expected_version":old["version"],"parent_id":null,"name":"并发文件夹"});
    let (one, two) = tokio::join!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/wordlist-library/owned/folders",
            create.clone()
        ),
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/wordlist-library/owned/folders",
            create
        )
    );
    assert!([one.0, two.0].contains(&StatusCode::NO_CONTENT));
    assert!([one.0, two.0].contains(&StatusCode::CONFLICT));
    assert_eq!(
        call(&state, &auth, "POST", endpoint, make("trash", Value::Null))
            .await
            .0,
        StatusCode::CONFLICT
    );
    op(
        &state,
        &auth,
        "owned",
        "trash",
        vec![a.clone()],
        Value::Null,
    )
    .await;
    folder(&state, &auth, "owned", Value::Null, "英语").await;
    op(
        &state,
        &auth,
        "owned",
        "restore",
        vec![a.clone()],
        Value::Null,
    )
    .await;
    assert!(
        listing(&state, &auth, "owned", "").await["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["name"] == "英语（恢复）")
    );
}
#[sqlx::test]
async fn library_favorites_preserve_references_and_publication_boundary(pool: PgPool) {
    let (state, owner) = setup_bound(&pool).await;
    let (_, reader) = setup_bound(&pool).await;
    let e = entry(&pool, "apple").await;
    let (_, w) = call(
        &state,
        &owner,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[e]),
    )
    .await;
    let id = w["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let target = folder(&state, &reader, "favorites", Value::Null, "备考").await;
    let interaction = format!("/api/v1/me/wordlists/{id}/interactions");
    let (s, b) = call(
        &state,
        &reader,
        "PUT",
        &interaction,
        json!({"kind":"favorite","active":true,"folder_id":target}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let f = listing(
        &state,
        &reader,
        "favorites",
        &format!("?parent_id={}", target.as_str().unwrap()),
    )
    .await["items"][0]["id"]
        .clone();
    let source = listing(&state, &owner, "owned", "").await["items"][0]["id"].clone();
    op(
        &state,
        &owner,
        "owned",
        "trash",
        vec![source.clone()],
        Value::Null,
    )
    .await;
    let public = format!("/api/v1/wordlists/{id}");
    assert_eq!(
        call(&state, &reader, "GET", &public, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    let fav = listing(
        &state,
        &reader,
        "favorites",
        &format!("?parent_id={}", target.as_str().unwrap()),
    )
    .await;
    assert!(fav["items"][0]["wordlist"].is_null());
    let legacy = call(
        &state,
        &reader,
        "GET",
        "/api/v1/me/wordlist-favorites",
        Value::Null,
    )
    .await;
    assert_eq!(legacy.1["items"][0]["wordlist_id"], w["id"]);
    assert!(legacy.1["items"][0]["wordlist"].is_null());
    op(
        &state,
        &reader,
        "favorites",
        "trash",
        vec![target.clone()],
        Value::Null,
    )
    .await;
    assert_eq!(
        call(
            &state,
            &reader,
            "GET",
            &format!("/api/v1/me/wordlist-interactions?ids={id}"),
            Value::Null
        )
        .await
        .1["items"][0]["favorited"],
        false
    );
    op(
        &state,
        &reader,
        "favorites",
        "restore",
        vec![f.clone()],
        Value::Null,
    )
    .await;
    assert_eq!(
        call(
            &state,
            &reader,
            "GET",
            &format!("/api/v1/me/wordlist-interactions?ids={id}"),
            Value::Null
        )
        .await
        .1["items"][0]["favorited"],
        true
    );
    op(
        &state,
        &owner,
        "owned",
        "restore",
        vec![source],
        Value::Null,
    )
    .await;
    assert_eq!(
        call(&state, &reader, "GET", &public, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &state,
            &owner,
            "GET",
            &format!("/api/v1/me/wordlists/{id}"),
            Value::Null
        )
        .await
        .1["state"],
        "draft"
    );
    op(
        &state,
        &reader,
        "favorites",
        "trash",
        vec![f.clone()],
        Value::Null,
    )
    .await;
    let v = listing(&state, &reader, "favorites", "").await;
    let (s, b) = call(
        &state,
        &reader,
        "POST",
        "/api/v1/me/wordlist-library/favorites/operations",
        json!({"expected_version":v["version"],"action":"purge","ids":[],"empty_trash":true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        call(
            &state,
            &owner,
            "GET",
            &format!("/api/v1/me/wordlists/{id}/items"),
            Value::Null
        )
        .await
        .1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[sqlx::test]
async fn library_rename_waiting_for_a_wordlist_rechecks_account_deadline(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let entry = entry(&pool, "deadline").await;
    let (_, created) = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[entry]),
    )
    .await;
    let id = created["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    let before = listing(&state, &auth, "owned", "").await;
    let node = before["items"][0]["id"].as_str().unwrap().to_owned();
    let request = apply(&state, &auth, "0").await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM wordlists WHERE id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let state2 = state.clone();
    let auth2 = AuthUser {
        subject: auth.subject,
        role: auth.role.clone(),
        security_version: auth.security_version,
    };
    let writer = tokio::spawn(async move {
        call(
            &state2,
            &auth2,
            "PUT",
            &format!("/api/v1/me/wordlist-library/owned/nodes/{node}"),
            json!({"expected_version":before["version"],"name":"too late"}),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    deadline(&pool, request.id, -1).await;
    blocker.commit().await.unwrap();
    assert_eq!(writer.await.unwrap().0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT name FROM wordlists WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "我的词表"
    );
}
