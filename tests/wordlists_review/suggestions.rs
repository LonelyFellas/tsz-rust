use super::*;

async fn surface(pool: &PgPool, id: Uuid, label: &str, scope: &str, publication: Option<Uuid>) {
    let (current, revision): (Uuid, i64) =
        sqlx::query_as("SELECT current_publication_id,revision FROM lexicon.entries WHERE id=$1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    let variant = Uuid::now_v7();
    let normalized = tsz_rust::lexicon::normalization::normalize_headword(label).unwrap();
    sqlx::query("INSERT INTO lexicon.surface_sources(entry_id,source_id,source_kind,source_node_id,language,entry_kind,dialect,dialect_scope,surface,normalized_surface,normalization_version,source_revision,content_scope,publication_id,pos_id,pos,form_type,content_schema_version,form_id,variant_id,group_ids,projection_version) VALUES($1,$2,'form_variant',$3,'en','word','common',$4,$5,$6,1,$7,'current_publication',$8,$9,'noun','base',3,$10,$3,ARRAY[$11]::uuid[],'v3-test')")
        .bind(id).bind(variant.to_string()).bind(variant).bind(scope).bind(label).bind(normalized.key).bind(revision).bind(publication.unwrap_or(current)).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(pool).await.unwrap();
}
#[sqlx::test]
async fn suggestions_rank_words_dedupe_surfaces_and_return_only_lightweight_summaries(
    pool: PgPool,
) {
    let (state, user) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let apple = full_entry(&pool, "apple").await;
    surface(&pool, apple, "apple", "uk", None).await;
    surface(&pool, apple, "apple", "us", None).await;
    surface(&pool, apple, "apples", "uk", None).await;
    for label in ["application", "apply", "pineapple", "a", "look forward to"] {
        let id = entry(&pool, label).await;
        surface(&pool, id, label, "uk", None).await;
    }
    let apostrophe = entry(&pool, "It’s working").await;
    surface(&pool, apostrophe, "It’s working", "uk", None).await;
    for label in ["look forward for", "look forward at"] {
        let id = entry(&pool, label).await;
        for _ in 0..40 {
            surface(&pool, id, label, "uk", None).await;
        }
    }
    let get = |q: &str| format!("/api/v1/wordlists/suggestions?q={q}");
    let prefix = call(&state, &user, "GET", &get("appl"), Value::Null).await;
    assert_eq!(prefix.0, StatusCode::OK, "{}", prefix.1);
    assert_eq!(
        prefix.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["label"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["apple", "application", "apply"]
    );
    assert!(prefix.1.get("pagination").is_none());
    let item = &prefix.1["items"][0];
    assert_eq!(item.as_object().unwrap().len(), 4);
    assert_eq!(item["glosses"], json!(["测试中文", "专用词义"]));
    assert!(!prefix.1.to_string().contains("ADMIN PRIVATE"));
    let exact = call(&state, &user, "GET", &get("APPLE"), Value::Null).await;
    assert_eq!(exact.1["items"][0]["entry_id"], apple.to_string());
    let short = call(&state, &user, "GET", &get("a"), Value::Null).await;
    assert_eq!(short.1["items"].as_array().unwrap().len(), 1);
    assert_eq!(short.1["items"][0]["label"], "a");
    let short_none = call(&state, &user, "GET", &get("ap"), Value::Null).await;
    assert_eq!(short_none.1["items"], json!([]));
    let typo = call(&state, &user, "GET", &get("aple"), Value::Null).await;
    assert!(
        typo.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["entry_id"] == apple.to_string())
    );
    assert!(typo.1["items"].as_array().unwrap().len() <= 3);
    let phrase = call(&state, &user, "GET", &get("look%20forward"), Value::Null).await;
    assert!(
        phrase.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["label"].as_str().unwrap().starts_with("look forward"))
    );
    let phrase_typo = call(&state, &user, "GET", &get("look%20forwadr"), Value::Null).await;
    assert_eq!(phrase_typo.0, StatusCode::OK, "{}", phrase_typo.1);
    let phrase_ids = phrase_typo.1["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["entry_id"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        phrase_ids.len(),
        3,
        "duplicates must not crowd out other entries: {}",
        phrase_typo.1
    );
    let alias = call(&state, &user, "GET", &get("it's%20wor"), Value::Null).await;
    assert_eq!(alias.1["items"][0]["entry_id"], apostrophe.to_string());
    let normalized = call(
        &state,
        &user,
        "GET",
        &get("%EF%BC%A1%EF%BC%B0%EF%BC%B0%EF%BC%AC%EF%BC%A5"),
        Value::Null,
    )
    .await;
    assert_eq!(normalized.1["items"][0]["entry_id"], apple.to_string());
    for q in ["", "%20%20", "%25", "%5F%5F", "app%25", "zzxxyyqq"] {
        let response = call(&state, &user, "GET", &get(q), Value::Null).await;
        assert_eq!(response.0, StatusCode::OK, "{}", response.1);
        assert_eq!(response.1["items"], json!([]));
    }
    assert_eq!(
        call(&state, &user, "GET", &get("a&page=2"), Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let public = call(
        &state,
        &user,
        "GET",
        "/api/v1/wordlists/suggestions?q=appl",
        Value::Null,
    )
    .await;
    assert_eq!(public, prefix);
    // Past publications must never remain a candidate after a publication swap.
    let old: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id=$1")
            .bind(apple)
            .fetch_one(&pool)
            .await
            .unwrap();
    publish(&pool, apple, manager, "renamed", 3).await;
    surface(&pool, apple, "retired-key", "uk", Some(old)).await;
    assert_eq!(
        call(&state, &user, "GET", &get("retired-key"), Value::Null)
            .await
            .1["items"],
        json!([])
    );
    sqlx::query("UPDATE lexicon.entries SET archived_at=now() WHERE id=$1")
        .bind(apple)
        .execute(&pool)
        .await
        .unwrap();
    let archived = call(&state, &user, "GET", &get("apple"), Value::Null).await;
    assert!(
        !archived.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["entry_id"] == apple.to_string())
    );
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(user.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &user, "GET", &get("apple"), Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}
