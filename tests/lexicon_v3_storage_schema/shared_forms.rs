use super::*;
use serde_json::Value;

const UP: &str = include_str!("../../migrations/20261020090000_shared_form_contents.up.sql");
const DOWN: &str = include_str!("../../migrations/20261020090000_shared_form_contents.down.sql");
const MAPPING_UP: &str =
    include_str!("../../migrations/20261020220000_form_pronunciation_slots.up.sql");
const MAPPING_DOWN: &str =
    include_str!("../../migrations/20261020220000_form_pronunciation_slots.down.sql");

#[sqlx::test]
async fn shared_forms_migration_preserves_old_nodes_duplicate_spelling_and_publication_hash(
    pool: PgPool,
) {
    sqlx::raw_sql(MAPPING_DOWN).execute(&pool).await.unwrap();
    sqlx::raw_sql(DOWN).execute(&pool).await.unwrap();
    let admin = insert_admin(&pool).await;
    let entry = insert_v3_entry(&pool, admin).await;
    let noun = catalog_pos_id(&pool, "noun").await;
    let mut tx = pool.begin().await.unwrap();
    let pos = insert_v3_pos(&mut tx, entry, noun, 0).await;
    let group = insert_v3_group(&mut tx, entry, pos, 0).await;
    let mut forms = Vec::new();
    let mut members = Vec::new();
    for ordinal in 0..2 {
        let (form, member, variant) =
            insert_valid_common_form(&mut tx, entry, pos, group, "base", ordinal, ordinal).await;
        forms.push(json!({"id":form,"form_type":"base","regional_variants":{"mode":"common","common":{"id":variant,"dialect":"common","spelling":"word","origin":"manual","pronunciations":[]}}}));
        members.push(json!({"id":member,"form_id":form}));
    }
    let old = json!({"pos":[{"pos_id":pos,"pos":"noun","forms":forms,"form_groups":[{"id":group,"is_regular":false,"scope":"general","dialect_rules":{"spelling_mode":"unified","phonetic_mode":"unified"},"members":members}]}]});
    sqlx::query("INSERT INTO lexicon.entry_editor_projection(entry_id,forms,meanings,rebuilt_revision) VALUES($1,$2,'{}',1)").bind(entry).bind(&old).execute(&mut *tx).await.unwrap();
    let publication = Uuid::now_v7();
    let snapshot = json!({"forms":old,"legacy":true});
    let hash = vec![1_u8, 2, 3, 4];
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,1,1,3,$3,$4,$5)").bind(publication).bind(entry).bind(&snapshot).bind(&hash).bind(admin).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let before: Vec<(Uuid, Option<Uuid>, String, bool)> = sqlx::query_as("SELECT id,parent_node_id,node_role,stable_slot FROM lexicon.nodes WHERE entry_id=$1 ORDER BY id").bind(entry).fetch_all(&pool).await.unwrap();
    sqlx::raw_sql(UP).execute(&pool).await.unwrap();
    sqlx::raw_sql(MAPPING_UP).execute(&pool).await.unwrap();
    let migrated: Value =
        sqlx::query_scalar("SELECT forms FROM lexicon.entry_editor_projection WHERE entry_id=$1")
            .bind(entry)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(migrated["forms_format"], "shared_v1");
    assert_eq!(migrated["contents"].as_array().unwrap().len(), 2);
    assert_ne!(
        migrated["pos"][0]["forms"][0]["content_id"],
        migrated["pos"][0]["forms"][1]["content_id"]
    );
    let parsed: tsz_rust::lexicon::dto::DraftFormsStepContentV3 =
        serde_json::from_value(migrated).unwrap();
    assert_eq!(parsed.pos[0].forms.len(), 2);
    assert_eq!(
        parsed.pos[0].forms[0].id.to_string(),
        old["pos"][0]["forms"][0]["id"].as_str().unwrap()
    );
    let after: Vec<(Uuid, Option<Uuid>, String, bool)> = sqlx::query_as("SELECT id,parent_node_id,node_role,stable_slot FROM lexicon.nodes WHERE entry_id=$1 ORDER BY id").bind(entry).fetch_all(&pool).await.unwrap();
    assert_eq!(before, after);
    let frozen: (Value, Vec<u8>) =
        sqlx::query_as("SELECT snapshot,snapshot_hash FROM lexicon.entry_publications WHERE id=$1")
            .bind(publication)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(frozen, (snapshot, hash));
    let rejected = sqlx::raw_sql(DOWN).execute(&pool).await.unwrap_err();
    assert!(rejected.to_string().contains("lossy rollback"));
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT to_regclass('lexicon.v3_form_contents') IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap()
    );
}

#[sqlx::test]
async fn shared_forms_foreign_keys_reject_cross_entry_bindings_and_slot_parent(pool: PgPool) {
    let admin = insert_admin(&pool).await;
    let first = insert_v3_entry(&pool, admin).await;
    let second = insert_v3_entry(&pool, admin).await;
    let noun = catalog_pos_id(&pool, "noun").await;
    let mut tx = pool.begin().await.unwrap();
    let pos = insert_v3_pos(&mut tx, first, noun, 0).await;
    let group = insert_v3_group(&mut tx, first, pos, 0).await;
    let (form, _, _) = insert_valid_common_form(&mut tx, first, pos, group, "base", 0, 0).await;
    let content = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.v3_form_contents(id,entry_id,content) VALUES($1,$2,'{}')")
        .bind(content)
        .bind(second)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO lexicon.v3_form_content_bindings(form_id,entry_id,content_id,regional_bindings) VALUES($1,$2,$3,'{}')").bind(form).bind(first).bind(content).execute(&mut *tx).await.unwrap();
    let error = tx.commit().await.unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some(FOREIGN_KEY_VIOLATION)
    );
    sqlx::query("INSERT INTO lexicon.v3_form_content_slots(id,entry_id,content_id,role) VALUES($1,$2,$1,'content')").bind(content).bind(second).execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO lexicon.v3_form_content_slots(id,entry_id,content_id,parent_slot_id,role) VALUES($1,$2,$3,$3,'common')").bind(Uuid::now_v7()).bind(first).bind(content).execute(&mut *tx).await.unwrap();
    let error = tx.commit().await.unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some(FOREIGN_KEY_VIOLATION)
    );
}
