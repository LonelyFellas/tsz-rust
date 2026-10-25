use super::generated_source::{GeneratedSpeechSource, references};
use serde_json::json;
use uuid::Uuid;

#[sqlx::test]
async fn historical_self_links_use_supplied_forms_and_external_links_hold_a_share_lock(
    pool: sqlx::PgPool,
) {
    use crate::admin::{AdminRepository, AdminRole, NewAdmin};
    let actor = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id: actor,
            phone: format!("voice-history-{actor}"),
            display_name: "voice history".into(),
            password_hash: "hash".into(),
            role: AdminRole::SuperAdmin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    let entry = Uuid::now_v7();
    let pos = Uuid::now_v7();
    let form = Uuid::now_v7();
    let variant = Uuid::now_v7();
    let forms = |phoneme: &str| json!({"pos":[{"pos_id":pos,"pos":"noun","form_groups":[],"forms":[{"id":form,"form_type":"base","regional_variants":{"mode":"common","common":{"id":variant,"dialect":"common","spelling":"far","origin":"manual","pronunciations":[{"id":Uuid::now_v7(),"dict_phonetic":"","actual_pron":"","synthesis":{"alphabet":"ipa","ipa":"","ups":"","uk":{"ipa":phoneme,"ups":""}}}]}}}]}]});
    let historical = serde_json::from_value(forms("fɑː")).unwrap();
    sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,revision,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word',1,'{}',$2,$2)").bind(entry).bind(actor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO lexicon.entry_editor_projection(entry_id,forms,meanings,rebuilt_revision) VALUES($1,$2,'{\"sense_groups\":[],\"pos\":[]}',1)").bind(entry).bind(forms("fɑɹ")).execute(&pool).await.unwrap();
    let source: GeneratedSpeechSource = serde_json::from_value(json!({"kind":"text","content":{"version":2,"text":"far","annotations":[]},"form_links":[{"id":Uuid::now_v7(),"source_segments":[{"start":0,"end":3,"surface":"far"}],"target_word_id":entry,"target_pos_id":pos,"target_form_id":form,"target_variant_id":variant,"target_dialect":"common"}]})).unwrap();
    let mut tx = pool.begin().await.unwrap();
    let resolved = source
        .resolve_with_forms(&mut tx, "en-GB", Some((entry, &historical)))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(resolved).unwrap()["annotations"][0]["phoneme"],
        "fɑː"
    );
    let current = source.resolve(&mut tx, "en-GB").await.unwrap();
    assert_eq!(
        serde_json::to_value(current).unwrap()["annotations"][0]["phoneme"],
        "fɑɹ"
    );
    let mut writer = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout='50ms'")
        .execute(&mut *writer)
        .await
        .unwrap();
    let blocked = sqlx::query("UPDATE lexicon.entries SET revision=revision+1 WHERE id=$1")
        .bind(entry)
        .execute(&mut *writer)
        .await
        .unwrap_err();
    assert_eq!(
        blocked
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("55P03")
    );
    writer.rollback().await.unwrap();
    tx.rollback().await.unwrap();
}

#[test]
fn ipa_reference_ignores_neighboring_ups_word_data_and_accepts_absent_profiles() {
    let source = json!({"version":2,"text":"hello","annotations":[]});
    let version = json!({"version_id":Uuid::now_v7(),"version_name":"自然","locale":"en-GB","voice_id":"sonia","enabled":true,"rate_percent":0,"audio_asset_id":Uuid::now_v7()});
    let owner = json!({"id":Uuid::now_v7(),"voice_profile":null,"synthesis":{"uk":{"ipa":"həˈləʊ","ups":"HH AH L OW","ups_words":[{"text":"hello","phoneme":"HH AH L OW"}],"ipa_speech":{"content":source,"voice_profile":{"voices":[version]}}}}});
    let refs = references(&owner).unwrap();
    assert_eq!(refs.len(), 1);
    let request: GeneratedSpeechSource = serde_json::from_value(
        json!({"kind":"phoneme","content":source,"alphabet":"ipa","phoneme":"həˈləʊ"}),
    )
    .unwrap();
    assert_eq!(
        refs[0].source.canonical().unwrap(),
        request.canonical().unwrap()
    );
}
