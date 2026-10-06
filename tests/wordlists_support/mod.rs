#![allow(dead_code)]
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
pub async fn admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO admins(id,phone,display_name,password_hash,role,must_change_password) VALUES($1,$2,'Reviewer','hash','super_admin',false)").bind(id).bind(format!("wl-{id}")).execute(pool).await.unwrap();
    id
}
pub async fn entry(pool: &PgPool, label: &str) -> Uuid {
    let admin = admin(pool).await;
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word','{}',$2,$2)").bind(id).bind(admin).execute(pool).await.unwrap();
    publish(pool, id, admin, label, 1).await;
    id
}
pub async fn publish(pool: &PgPool, id: Uuid, admin: Uuid, label: &str, n: i32) {
    let p = Uuid::now_v7();
    let pos = Uuid::now_v7();
    let grammar = Uuid::now_v7();
    let snapshot = json!({"schema_version":3,"id":id,"language":"en","kind":"word","status":"published","revision":n,"lifecycle_revision":1,"published_revision":n,"has_unpublished_changes":false,"presentation":{"label":label,"matched_surfaces":[label],"strategy_version":"test"},"capabilities":{"publication":{"mode":"native"},"pronunciation_normalization_version":"nfkc_trim_lower_v1"},"forms":{"pos":[{"pos_id":pos,"pos":"noun","forms":[],"form_groups":[]}]},"meanings":{"sense_groups":[],"pos":[{"pos_id":pos,"grammar_structures":[{"id":grammar,"variants":[{"id":Uuid::now_v7(),"dialect":"common","content":{"version":1,"text":"a + noun","spans":[],"liaisons":[]}}]}],"senses":[{"id":Uuid::now_v7(),"sub_pos":"countable","level":"C2","depends_on_context":false,"definitions":[{"id":Uuid::now_v7(),"content_id":Uuid::now_v7(),"definition_mode":"zh_definition","level":"A1","grammar_structure_id":grammar,"content":{"version":1,"text":"测试中文","spans":[],"liaisons":[]}}],"sentences":[],"relations":[]}]}]},"completed_steps":["forms","meanings"],"max_reachable_step":"preview","created_by":admin,"created_at":"2026-10-06T00:00:00Z","updated_at":"2026-10-06T00:00:00Z","annotation":"ADMIN PRIVATE"});
    // This fixture exercises only immutable publication reads, not the lexicon publish workflow.
    let _: tsz_rust::lexicon::dto::AdminWordV3 = serde_json::from_value(snapshot.clone()).unwrap();
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,$3,$4,3,$5,$6,$7)").bind(p).bind(id).bind(n).bind(i64::from(n)).bind(snapshot).bind(p.as_bytes().to_vec()).bind(admin).execute(pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=$2 WHERE id=$1")
        .bind(id)
        .bind(p)
        .execute(pool)
        .await
        .unwrap();
}
pub fn create_body(ids: &[Uuid]) -> Value {
    json!({"idempotency_key":Uuid::now_v7(),"name":"我的词表","items":ids.iter().map(|id|json!({"entry_id":id,"private_note":"私密备注"})).collect::<Vec<_>>()})
}
pub async fn admin_call(
    state: &tsz_rust::state::AppState,
    admin: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (axum::http::StatusCode, Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let token = state
        .admin_token_manager
        .generate_with_version(admin, "super_admin", 0)
        .unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let data = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&data).unwrap_or(Value::Null))
}
