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

pub async fn share_current_publication(pool: &PgPool, entry_id: Uuid) {
    let mut snapshot: Value = sqlx::query_scalar("SELECT p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(entry_id).fetch_one(pool).await.unwrap();
    let mut contents = Vec::new();
    for pos in snapshot["forms"]["pos"].as_array_mut().unwrap() {
        let groups = pos["form_groups"].as_array().unwrap().clone();
        for form in pos["forms"].as_array_mut().unwrap() {
            let content_id = Uuid::now_v7();
            let group = groups
                .iter()
                .find(|group| {
                    group["members"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|member| member["form_id"] == form["id"])
                })
                .unwrap();
            let mut regions = form
                .as_object_mut()
                .unwrap()
                .remove("regional_variants")
                .unwrap();
            let mut bindings = json!({"mode":regions["mode"]});
            for side in ["common", "uk", "us"] {
                if let Some(variant) = regions.get_mut(side) {
                    let reference_id = variant["id"].clone();
                    let slot_id = Uuid::now_v7();
                    let pronunciations: Vec<_> = variant["pronunciations"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .map(|row| {
                            let id = row["id"].clone();
                            let slot = Uuid::now_v7();
                            row["id"] = json!(slot);
                            json!({"id":id,"content_slot_id":slot})
                        })
                        .collect();
                    variant["id"] = json!(slot_id);
                    let object = variant.as_object_mut().unwrap();
                    object.remove("dialect");
                    let regular = object
                        .remove("is_regular")
                        .unwrap_or_else(|| group["is_regular"].clone());
                    let usages = object.remove("component_usages");
                    bindings[side] = json!({"id":reference_id,"content_slot_id":slot_id,"is_regular":regular,"pronunciations":pronunciations});
                    if let Some(usages) = usages {
                        bindings[side]["component_usages"] = usages;
                    }
                }
            }
            contents.push(json!({"id":content_id,"dialect_rules":group["dialect_rules"],"regional_variants":regions}));
            form["content_id"] = json!(content_id);
            form["regional_bindings"] = bindings;
        }
        for group in pos["form_groups"].as_array_mut().unwrap() {
            group.as_object_mut().unwrap().remove("dialect_rules");
        }
    }
    // A second grammatical use shares the first content, with independent reference nodes.
    let mut form = snapshot["forms"]["pos"][0]["forms"][0].clone();
    let id = Uuid::now_v7();
    form["id"] = json!(id);
    form["form_type"] = json!("plural");
    for side in ["common", "uk", "us"] {
        if let Some(variant) = form["regional_bindings"].get_mut(side) {
            variant["id"] = json!(Uuid::now_v7());
            for row in variant["pronunciations"].as_array_mut().unwrap() {
                row["id"] = json!(Uuid::now_v7());
            }
        }
    }
    snapshot["forms"]["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .push(form);
    snapshot["forms"]["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":Uuid::now_v7(),"form_id":id}));
    snapshot["forms"]["forms_format"] = json!("shared_v1");
    snapshot["forms"]["contents"] = json!(contents);
    let _: tsz_rust::lexicon::dto::SharedFormsStepContentV3 =
        serde_json::from_value(snapshot["forms"].clone()).unwrap();
    let _: tsz_rust::lexicon::dto::AdminWordV3 = serde_json::from_value(snapshot.clone()).unwrap();
    // Replace only this test fixture's current publication; production snapshots remain immutable.
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(entry_id).bind(snapshot).execute(pool).await.unwrap();
}

/// A published read fixture with multiple bases, dialects and dedicated sense bindings.
pub async fn full_entry(pool: &PgPool, label: &str) -> Uuid {
    let id = entry(pool, label).await;
    let mut snapshot: Value = sqlx::query_scalar("SELECT p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(id).fetch_one(pool).await.unwrap();
    let rich = |text: &str| json!({"version":1,"text":text,"spans":[],"liaisons":[]});
    let pronunciation = |text: &str| json!({"id":Uuid::now_v7(),"dict_phonetic":text,"dict_phonetic_rich":rich(text),"actual_pron":"PRIVATE ACTUAL","style":"normal"});
    let regional = |dialect: &str, spelling: &str, sounds: Vec<Value>| json!({"id":Uuid::now_v7(),"dialect":dialect,"spelling":spelling,"origin":"manual","pronunciations":sounds});
    let base = Uuid::now_v7();
    let alternate = Uuid::now_v7();
    let plural = Uuid::now_v7();
    let general = Uuid::now_v7();
    let dedicated = Uuid::now_v7();
    snapshot["forms"]["pos"][0]["forms"] = json!([
        {"id":base,"form_type":"base","regional_variants":{"mode":"uk_us","uk":regional("uk",label,vec![pronunciation("/riːd/"),pronunciation("/red/")]),"us":regional("us",label,vec![pronunciation("ɹiːd")])}},
        {"id":alternate,"form_type":"base","regional_variants":{"mode":"uk_us","uk":regional("uk","alternate-base",vec![pronunciation("ɔːlt")]),"us":regional("us","alternate-base",vec![pronunciation("ɔlt")])}},
        {"id":plural,"form_type":"plural","regional_variants":{"mode":"common","common":regional("common","dedicated-plural",vec![pronunciation("plʊərəl")])}}
    ]);
    let group = |id: Uuid, scope: &str, forms: &[Uuid]| json!({"id":id,"scope":scope,"is_regular":true,"dialect_rules":{"spelling_mode":"unified","phonetic_mode":if scope == "general" { "distinguish" } else { "unified" }},"members":forms.iter().map(|form|json!({"id":Uuid::now_v7(),"form_id":form})).collect::<Vec<_>>()});
    snapshot["forms"]["pos"][0]["form_groups"] = json!([
        group(general, "general", &[base, alternate]),
        group(dedicated, "dedicated", &[plural])
    ]);
    snapshot["meanings"]["pos"][0]["senses"][0]["sub_pos"] = json!("N-COUNT");
    let mut second = snapshot["meanings"]["pos"][0]["senses"][0].clone();
    second["id"] = json!(Uuid::now_v7());
    second["form_group_ids"] = json!([dedicated]);
    second["definitions"][0]["id"] = json!(Uuid::now_v7());
    second["definitions"][0]["content_id"] = json!(Uuid::now_v7());
    second["definitions"][0]["content"] = rich("专用词义");
    snapshot["meanings"]["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let _: tsz_rust::lexicon::dto::AdminWordV3 = serde_json::from_value(snapshot.clone()).unwrap();
    let publisher = admin(pool).await;
    let publication = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,2,2,3,$3,$4,$5)").bind(publication).bind(id).bind(snapshot).bind(publication.as_bytes().to_vec()).bind(publisher).execute(pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=$2 WHERE id=$1")
        .bind(id)
        .bind(publication)
        .execute(pool)
        .await
        .unwrap();
    id
}
