use super::{language, model::*};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

/// Structural probes only: these future language modules are NOT accepted by the API.
/// Typed extensions refer to the shared form identity rather than English UK/US slots.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "language", deny_unknown_fields)]
enum FutureFormRules {
    #[serde(rename = "ja")]
    Japanese {
        form_id: Uuid,
        kana_reading: String,
        conjugation_class: String,
    },
    #[serde(rename = "fr")]
    French {
        form_id: Uuid,
        gender: String,
        number: String,
    },
    #[serde(rename = "es")]
    Spanish {
        form_id: Uuid,
        person: u8,
        number: String,
        tense: String,
    },
}

#[test]
fn representative_language_structures_preserve_core_identity_without_enabling_capabilities() {
    for (language_code, surface, rules) in [
        (
            "ja",
            "食べる",
            json!({"language":"ja","kana_reading":"たべる","conjugation_class":"ichidan"}),
        ),
        (
            "fr",
            "étudiante",
            json!({"language":"fr","gender":"feminine","number":"singular"}),
        ),
        (
            "es",
            "habló",
            json!({"language":"es","person":3,"number":"singular","tense":"preterite"}),
        ),
    ] {
        let id = Uuid::now_v7();
        let mut extension = rules;
        extension["form_id"] = json!(id);
        let typed: FutureFormRules = serde_json::from_value(extension.clone()).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), extension);
        let core = NextDraft {
            grammar_structures: None,
            sense_groups: None,
            form_editor: None,
            schema_version: 1,
            headword: surface.into(),
            forms: vec![NextForm {
                id,
                locale: language_code.into(),
                text: surface.into(),
            }],
            senses: vec![NextSense {
                relations: None,
                sentence_ids: None,
                no_equivalents: vec![],
                level: None,
                frequency: None,
                depends_on_context: None,
                sub_pos: None,
                sense_group_id: None,
                id: Uuid::now_v7(),
                part_of_speech: None,
                definitions: vec![NextDefinition {
                    level: None,
                    grammar_unbound: None,
                    content: None,
                    voice_profile: None,
                    audio_assets: vec![],
                    grammar_structure_id: None,
                    id: Uuid::now_v7(),
                    locale: "zh-Hant".into(),
                    text: "結構樣例，非已開放內容".into(),
                    style: NextDefinitionStyle::Definition,
                }],
                equivalents: vec![],
            }],
        };
        let serialized = serde_json::to_value(&core).unwrap();
        let restored: NextDraft = serde_json::from_value(serialized.clone()).unwrap();
        assert_eq!(restored.forms[0].id, id);
        assert_eq!(restored.forms[0].text, surface); // no Latin-only validator or accent stripping in common storage
        assert_eq!(restored.senses[0].definitions[0].locale, "zh-Hant");
        assert!(language::validate(language_code, &restored).is_err());
        // Future typed modules must be versioned deliberately; no unvalidated extension bag today.
        let mut invalid = serialized;
        invalid["forms"][0]["language_rules"] = extension;
        assert!(serde_json::from_value::<NextDraft>(invalid).is_err());
    }
    let caps = language::capabilities();
    assert_eq!(caps.languages.len(), 1);
    assert_eq!(caps.languages[0].target_language, "en");
    assert!(!caps.generation && caps.publishing);
}

#[test]
fn openapi_declares_closed_drafts_and_distinct_endpoints() {
    use utoipa::OpenApi;
    let spec = serde_json::to_value(crate::openapi::ApiDoc::openapi()).unwrap();
    for schema in [
        "NextEntry",
        "NextDraft",
        "NextSense",
        "NextDefinition",
        "NextForm",
        "NextEquivalent",
        "CreateNextEntry",
        "SaveNextDraft",
    ] {
        assert_eq!(
            spec["components"]["schemas"][schema]["additionalProperties"], false,
            "{schema}"
        );
    }
    let paths = &spec["paths"];
    assert!(paths["/api/v1/admin/lexicon-next/entries"]["post"].is_object());
    assert!(paths["/api/v1/admin/lexicon-next/entries/{id}"]["put"].is_object());
    assert!(
        paths["/api/v1/admin/lexicon-next/entries/{id}"]["put"]["responses"]["409"].is_object()
    );
}

#[test]
fn sense_groups_keep_draft_names_and_validate_identity_membership_and_closed_wire() {
    let group = Uuid::now_v7();
    let sense = Uuid::now_v7();
    let raw = json!({"schema_version":1,"headword":"centre","forms":[],"sense_groups":[
        {"id":group,"name_zh":"","name_en":""}
    ],"senses":[{"id":sense,"part_of_speech":null,"sense_group_id":group,"definitions":[],"equivalents":[]}]});
    let valid: NextDraft = serde_json::from_value(raw.clone()).unwrap();
    language::validate("en", &valid).unwrap();
    assert_eq!(serde_json::to_value(valid).unwrap(), raw);
    for (field, value) in [("sense_groups", json!(null)), ("unknown", json!(true))] {
        let mut invalid = raw.clone();
        invalid[field] = value;
        assert!(serde_json::from_value::<NextDraft>(invalid).is_err());
    }
    let mut null_id = raw.clone();
    null_id["senses"][0]["sense_group_id"] = json!(null);
    assert!(serde_json::from_value::<NextDraft>(null_id).is_err());
    for invalid in [
        {
            let mut v = raw.clone();
            v["senses"][0]["sense_group_id"] = json!(Uuid::now_v7());
            v
        },
        {
            let mut v = raw.clone();
            v["sense_groups"][0]["id"] = json!(sense);
            v
        },
        {
            let mut v = raw.clone();
            v["sense_groups"][0]["name_zh"] = json!("字".repeat(201));
            v
        },
    ] {
        assert!(language::validate("en", &serde_json::from_value(invalid).unwrap()).is_err());
    }
}

#[test]
fn legacy_grammar_annotation_edits_invalidate_audio_without_losing_voice_settings() {
    let asset = Uuid::now_v7();
    let previous: NextDraft = serde_json::from_value(json!({
        "schema_version":1,"headword":"job","forms":[],"senses":[],
        "grammar_structures":[{"id":Uuid::now_v7(),"part_of_speech":"noun","variants":[{
            "id":Uuid::now_v7(),"locale":"en-GB","content":{"version":2,"text":"a job","annotations":[]},
            "voice_profile":{"voices":[{"voice_id":"saved-voice","enabled":true,"rate_percent":15,
                "version_id":Uuid::now_v7(),"version_name":"原配音","locale":"en-GB","audio_asset_id":asset}]}
        }]}]
    })).unwrap();
    let mut unchanged = previous.clone();
    super::grammar::strip_audio(unchanged.grammar_structures.as_mut());
    super::grammar::preserve_audio(&mut unchanged, &previous);
    assert_eq!(
        serde_json::to_value(&unchanged).unwrap(),
        serde_json::to_value(&previous).unwrap()
    );
    let mut changed = unchanged;
    super::grammar::strip_audio(changed.grammar_structures.as_mut());
    let variant = &mut changed.grammar_structures.as_mut().unwrap()[0].variants[0];
    if let crate::lexicon::dto::RichTextV3::V2(content) = &mut variant.content {
        content
            .annotations
            .push(crate::lexicon::dto::RichTextAnnotationV3::Pause {
                at: 2,
                duration_ms: 500,
            });
    }
    super::grammar::preserve_audio(&mut changed, &previous);
    let saved = serde_json::to_value(changed).unwrap();
    let variant = &saved["grammar_structures"][0]["variants"][0];
    assert_eq!(variant["content"]["text"], "a job");
    assert_eq!(variant["content"]["annotations"][0]["type"], "pause");
    assert_eq!(variant["voice_profile"]["voices"][0]["rate_percent"], 15);
    assert!(
        variant["voice_profile"]["voices"][0]
            .get("audio_asset_id")
            .is_none()
    );
}

#[test]
fn sense_metadata_uses_v1_numeric_rules_without_backfilling_legacy_drafts() {
    let raw = json!({"schema_version":1,"headword":"job","forms":[],"senses":[{
        "id":Uuid::now_v7(),"part_of_speech":"noun","definitions":[],"equivalents":[]
    }]});
    let legacy: NextDraft = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(legacy).unwrap(), raw);
    for frequency in ["0", "100", "0.01", "95.50", "100.00"] {
        let mut value = raw.clone();
        value["senses"][0]["level"] = json!("A1");
        value["senses"][0]["frequency"] = json!(frequency);
        value["senses"][0]["depends_on_context"] = json!(false);
        language::validate("en", &serde_json::from_value(value).unwrap()).unwrap();
    }
    for (field, invalid) in [
        ("level", "A0"),
        ("frequency", "100.01"),
        ("frequency", "-1"),
        ("frequency", "1.234"),
        ("frequency", "NaN"),
        ("frequency", "1e1"),
        ("frequency", ""),
    ] {
        let mut value = raw.clone();
        value["senses"][0][field] = json!(invalid);
        assert!(language::validate("en", &serde_json::from_value(value).unwrap()).is_err());
    }
    for field in ["level", "frequency", "depends_on_context"] {
        let mut value = raw.clone();
        value["senses"][0][field] = serde_json::Value::Null;
        assert!(serde_json::from_value::<NextDraft>(value).is_err());
    }
}

fn definition_draft() -> serde_json::Value {
    json!({"schema_version":1,"headword":"job","forms":[],"senses":[{
        "id":Uuid::now_v7(),"part_of_speech":"noun","level":"C2","definitions":[{
            "id":Uuid::now_v7(),"locale":"en-GB","text":"a job","style":"definition",
            "level":"A1","grammar_unbound":true,
            "content":{"version":2,"text":"a job","annotations":[{"type":"pause","at":1,"duration_ms":200}]}
        }],"equivalents":[]
    }]})
}

#[test]
fn definition_details_validate_independent_levels_projection_markup_and_scope() {
    let raw = definition_draft();
    for level in ["A1", "A2", "B1", "B2", "C1", "C2"] {
        let mut value = raw.clone();
        value["senses"][0]["definitions"][0]["level"] = json!(level);
        language::validate("en", &serde_json::from_value(value).unwrap()).unwrap();
    }
    for (field, value) in [
        ("level", json!("A0")),
        ("grammar_unbound", json!(false)),
        ("grammar_structure_id", json!(Uuid::now_v7())),
        ("text", json!("different")),
        ("locale", json!("zh-Hans")),
        (
            "content",
            json!({"version":2,"text":"a job","annotations":[{"type":"pause","at":999,"duration_ms":200}]}),
        ),
        (
            "voice_profile",
            json!({"voices":[],"dubbing":{"area":"grammar","uk":{"mode":"inherit"},"us":{"mode":"inherit"}}}),
        ),
        (
            "voice_profile",
            json!({"voices":[{"voice_id":"test","enabled":false,"rate_percent":0,"version_id":Uuid::now_v7(),"version_name":"US","locale":"en-US"}]}),
        ),
    ] {
        let mut invalid = raw.clone();
        invalid["senses"][0]["definitions"][0][field] = value;
        assert!(
            language::validate("en", &serde_json::from_value(invalid).unwrap()).is_err(),
            "{field}"
        );
    }
    for field in [
        "level",
        "grammar_unbound",
        "content",
        "voice_profile",
        "audio_assets",
    ] {
        let mut invalid = raw.clone();
        invalid["senses"][0]["definitions"][0][field] = json!(null);
        assert!(
            serde_json::from_value::<NextDraft>(invalid).is_err(),
            "{field}"
        );
    }
    let mut draft: NextDraft = serde_json::from_value(raw).unwrap();
    draft.grammar_structures = Some(vec![]);
    let issues = super::grammar::issues(&draft, &Default::default());
    assert!(
        !issues
            .iter()
            .any(|issue| issue.node_id == draft.senses[0].definitions[0].id)
    );
}

#[test]
fn legacy_definition_edits_preserve_hidden_metadata_and_invalidate_changed_source() {
    let mut raw = definition_draft();
    raw["senses"][0]["definitions"][0]["voice_profile"] = json!({"voices":[{
        "voice_id":"saved-voice","enabled":false,"rate_percent":15,
        "version_id":Uuid::now_v7(),"version_name":"原配音","locale":"en-GB","audio_asset_id":Uuid::now_v7()
    }]});
    let previous: NextDraft = serde_json::from_value(raw).unwrap();
    let mut unchanged = previous.clone();
    super::definitions::strip(&mut unchanged.senses[0].definitions[0]);
    super::definitions::preserve(&mut unchanged, &previous);
    assert_eq!(
        serde_json::to_value(&unchanged).unwrap(),
        serde_json::to_value(&previous).unwrap()
    );
    let mut changed = unchanged;
    super::definitions::strip(&mut changed.senses[0].definitions[0]);
    changed.senses[0].definitions[0].text = "a new job".into();
    super::definitions::preserve(&mut changed, &previous);
    language::validate("en", &changed).unwrap();
    let row = &changed.senses[0].definitions[0];
    assert_eq!(row.level.as_deref(), Some("A1"));
    assert_eq!(row.grammar_unbound, Some(true));
    let saved = serde_json::to_value(row).unwrap();
    assert_eq!(saved["content"]["text"], "a new job");
    assert_eq!(saved["content"]["annotations"], json!([]));
    assert_eq!(saved["voice_profile"]["voices"][0]["rate_percent"], 15);
    assert!(
        saved["voice_profile"]["voices"][0]
            .get("audio_asset_id")
            .is_none()
    );
}

#[test]
fn phrase_constituents_save_with_their_grammar_and_filter_by_pos() {
    let draft: NextDraft = serde_json::from_value(json!({
        "schema_version":1,"headword":"take off","forms":[],
        "grammar_structures":[{"id":Uuid::now_v7(),"part_of_speech":"phrase_verbal_constituent","variants":[]}],
        "senses":[{"id":Uuid::now_v7(),"part_of_speech":"phrase_verbal_constituent","definitions":[],"equivalents":[]}]
    })).unwrap();
    language::validate("en", &draft).unwrap();
    super::workspace::validate(&super::list_model::NextWorkspaceQuery {
        pos: Some("phrase_verbal_constituent".into()),
        ..Default::default()
    })
    .unwrap();
    let mut invalid = draft;
    invalid.senses[0].part_of_speech = Some("phrase_Invalid".into());
    assert!(language::validate("en", &invalid).is_err());
}
