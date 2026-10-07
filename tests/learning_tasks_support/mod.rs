#![allow(dead_code)]
use crate::{
    account_deletion_support::{call, setup_bound},
    wordlists_support,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tsz_rust::{auth::extract::AuthUser, state::AppState};
use uuid::Uuid;
pub async fn setup(pool: &PgPool) -> (AppState, AuthUser, Uuid) {
    let (state, auth) = setup_bound(pool).await;
    sqlx::query(
        "INSERT INTO student_profiles(user_id,cefr_level,english_variant) VALUES($1,'A1','BrE')",
    )
    .bind(auth.subject)
    .execute(pool)
    .await
    .unwrap();
    let entry = wordlists_support::full_entry(pool, "apple").await;
    let list = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        wordlists_support::create_body(&[entry]),
    )
    .await;
    assert_eq!(list.0, 200, "{}", list.1);
    (
        state,
        auth,
        Uuid::parse_str(list.1["id"].as_str().unwrap()).unwrap(),
    )
}
pub fn create_body(list: Uuid, kind: &str) -> Value {
    json!({"idempotency_key":Uuid::now_v7(),"name":"真实练习","task_type":kind,"wordlist_ids":[list],"daily_question_count":if kind=="daily"{json!(2)}else{Value::Null},"ends_at":null})
}
pub async fn create(state: &AppState, auth: &AuthUser, list: Uuid, kind: &str) -> Value {
    let r = call(
        state,
        auth,
        "POST",
        "/api/v1/me/learning-tasks",
        create_body(list, kind),
    )
    .await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
pub fn start_body(after: Option<&str>) -> Value {
    json!({"idempotency_key":Uuid::now_v7(),"expected_revision":1,"after_run_id":after})
}
pub async fn start(state: &AppState, auth: &AuthUser, task: &Value) -> Value {
    let r = call(
        state,
        auth,
        "POST",
        &format!(
            "/api/v1/me/learning-tasks/{}/runs",
            task["id"].as_str().unwrap()
        ),
        start_body(None),
    )
    .await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
pub async fn questions(state: &AppState, auth: &AuthUser, run: &Value) -> Value {
    let r = call(
        state,
        auth,
        "GET",
        &format!(
            "/api/v1/me/learning-runs/{}/questions",
            run["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
pub fn answer_body(q: &Value, answer: &str) -> Value {
    json!({"idempotency_key":Uuid::now_v7(),"question_id":q["id"],"answer":answer})
}
pub fn answer_path(run: &Value) -> String {
    format!(
        "/api/v1/me/learning-runs/{}/answers",
        run["id"].as_str().unwrap()
    )
}
