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
                sub_pos: None,
                sense_group_id: None,
                id: Uuid::now_v7(),
                part_of_speech: None,
                definitions: vec![NextDefinition {
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
    assert!(!caps.generation && !caps.publishing);
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
