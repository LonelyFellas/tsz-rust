use super::*;
use axum::{
    body::Body,
    http::{Request, header},
};
use http_body_util::BodyExt;
use tower::ServiceExt;
use tsz_rust::state::AppState;
use uuid::Uuid;

fn has_new_formats(value: &Value) -> bool {
    if value.get("version").and_then(Value::as_u64) == Some(2)
        && value
            .get("annotations")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| matches!(item["type"].as_str(), Some("bold" | "underline")))
            })
    {
        return true;
    }
    match value {
        Value::Object(fields) => fields.values().any(has_new_formats),
        Value::Array(items) => items.iter().any(has_new_formats),
        _ => false,
    }
}

async fn negotiated_read(state: &AppState, bearer: &str, path: &str, capable: bool) -> Value {
    let mut request = Request::builder().uri(path);
    if !bearer.is_empty() {
        request = request.header("Authorization", format!("Bearer {bearer}"));
    }
    if capable {
        request = request.header("X-TSZ-Sentence-Formatting", "v1");
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::VARY).unwrap(),
        "x-tsz-sentence-formatting"
    );
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[sqlx::test]
async fn sentence_formatting_negotiates_personal_review_and_public_wordlist_content(pool: PgPool) {
    let (state, user) = setup_bound(&pool).await;
    let entry = full_entry(&pool, "harbour").await;
    let mut snapshot: Value = sqlx::query_scalar("SELECT p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(entry).fetch_one(&pool).await.unwrap();
    let grammar = snapshot["meanings"]["pos"][0]["grammar_structures"][0]["id"].clone();
    let rich = json!({"version":2,"text":"a harbour","annotations":[{"type":"bold","start":2,"end":9},{"type":"underline","start":2,"end":9},{"type":"italic","start":2,"end":5}]});
    snapshot["meanings"]["pos"][0]["senses"][0]["definitions"].as_array_mut().unwrap().push(json!({
        "id":Uuid::now_v7(),"definition_mode":"en_definition","level":"A1","grammar_structure_id":grammar,
        "content":{"mode":"unified","common":{"id":Uuid::now_v7(),"origin":"manual","value":rich}}
    }));
    snapshot["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0]["content"] = rich;
    let _: tsz_rust::lexicon::dto::AdminWordV3 = serde_json::from_value(snapshot.clone()).unwrap();
    // Immutable read fixture: insert a new publication instead of altering earlier snapshots.
    let publication = Uuid::now_v7();
    let publisher = admin(&pool).await;
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,3,3,3,$3,$4,$5)")
        .bind(publication).bind(entry).bind(&snapshot).bind(publication.as_bytes().to_vec()).bind(publisher).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=$2 WHERE id=$1")
        .bind(entry)
        .bind(publication)
        .execute(&pool)
        .await
        .unwrap();
    let reviewer = admin(&pool).await;
    let user_token = state
        .token_manager
        .generate_with_version(user.subject, "student", user.security_version)
        .unwrap();
    let admin_token = state
        .admin_token_manager
        .generate_with_version(reviewer, "super_admin", 0)
        .unwrap();
    let (status, created) = call(
        &state,
        &user,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[entry]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap();
    let private_path = format!("/api/v1/me/wordlists/{id}/items?view=full");
    let old = negotiated_read(&state, &user_token, &private_path, false).await;
    let new = negotiated_read(&state, &user_token, &private_path, true).await;
    assert!(!has_new_formats(&old));
    assert!(has_new_formats(&new));
    assert_eq!(
        old["items"][0]["private_note"],
        new["items"][0]["private_note"]
    );
    let (status, pending) = call(
        &state,
        &user,
        "POST",
        &format!("/api/v1/me/wordlists/{id}/review-requests"),
        json!({"expected_revision":1,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    let request = pending["id"].as_str().unwrap();
    let review_path = format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/items");
    let old = negotiated_read(&state, &admin_token, &review_path, false).await;
    let new = negotiated_read(&state, &admin_token, &review_path, true).await;
    assert!(!has_new_formats(&old));
    assert!(has_new_formats(&new));
    assert!(!new.to_string().contains("private_note"));
    let (status, approved) = admin_call(
        &state,
        reviewer,
        "POST",
        &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision"),
        json!({"expected_revision":2,"approve":true,"reason":null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    let public_path = format!("/api/v1/wordlists/{id}/items?view=full");
    let old = negotiated_read(&state, "", &public_path, false).await;
    let new = negotiated_read(&state, "", &public_path, true).await;
    assert!(!has_new_formats(&old));
    assert!(has_new_formats(&new));
    assert!(!new.to_string().contains("private_note"));
    let stored: Value =
        sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id=$1")
            .bind(publication)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, snapshot);
}
