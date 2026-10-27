use axum::http::HeaderMap;
use serde_json::Value;
use std::collections::HashSet;

pub(crate) const HEADER: &str = "x-tsz-voice-versions";
pub(crate) const LOCALES_HEADER: &str = "x-tsz-voice-locales";
pub(crate) const MODELS_HEADER: &str = "x-tsz-voice-models";

pub(crate) fn models_supported(headers: &HeaderMap) -> bool {
    headers
        .get(MODELS_HEADER)
        .is_some_and(|value| value == "v1")
}

pub(crate) fn contains_models(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            ["voice_id", "voice_alias"].iter().any(|key| {
                fields
                    .get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(|alias| alias.starts_with("azure-hd-flash-"))
            }) || fields.values().any(contains_models)
        }
        Value::Array(items) => items.iter().any(contains_models),
        _ => false,
    }
}

pub(crate) fn project_models(value: &mut Value) {
    match value {
        Value::Object(fields) => fields.values_mut().for_each(project_models),
        Value::Array(items) => {
            items.retain(|item| {
                !["voice_id", "voice_alias"].iter().any(|key| {
                    item.get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|alias| alias.starts_with("azure-hd-flash-"))
                })
            });
            items.iter_mut().for_each(project_models);
        }
        _ => {}
    }
}
pub(crate) const UPGRADE_MESSAGE: &str =
    "此内容包含新版文本配音版本，请保留本地输入并刷新页面后再保存";

pub(crate) fn supported(headers: &HeaderMap) -> bool {
    headers
        .get(HEADER)
        .is_some_and(|value| value == "v1" || value == "v2" || value == "v3")
}

pub(crate) fn generated_supported(headers: &HeaderMap) -> bool {
    headers
        .get(HEADER)
        .is_some_and(|value| value == "v2" || value == "v3")
}

pub(crate) fn dubbing_supported(headers: &HeaderMap) -> bool {
    headers.get(HEADER).is_some_and(|value| value == "v3")
}

pub(crate) fn contains_dubbing(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            (fields.contains_key("voices") && fields.contains_key("dubbing"))
                || (fields.contains_key("voice_id")
                    && (fields.contains_key("style_degree")
                        || fields.contains_key("role")
                        || fields.contains_key("output_format")))
                || fields
                    .get("spelling_speech")
                    .and_then(|speech| speech.get("audio_assets"))
                    .and_then(Value::as_array)
                    .is_some_and(|assets| !assets.is_empty())
                || fields.contains_key("reading_reference")
                || fields.contains_key("reading_links")
                || fields
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "say_as" | "sub"))
                || fields.values().any(contains_dubbing)
        }
        Value::Array(items) => items.iter().any(contains_dubbing),
        _ => false,
    }
}

pub(crate) fn project_dubbing(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            if fields.contains_key("voices") {
                fields.remove("dubbing");
            }
            if let Some(speech) = fields
                .get_mut("spelling_speech")
                .and_then(Value::as_object_mut)
            {
                speech.remove("audio_assets");
            }
            if fields.contains_key("voice_id") {
                fields.remove("style_degree");
                fields.remove("role");
                fields.remove("output_format");
            }
            fields.remove("reading_reference");
            fields.remove("reading_links");
            fields.values_mut().for_each(project_dubbing);
        }
        Value::Array(items) => {
            items.retain(|item| {
                !item
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "say_as" | "sub"))
            });
            items.iter_mut().for_each(project_dubbing);
        }
        _ => {}
    }
}

pub(crate) fn extended_supported(headers: &HeaderMap) -> bool {
    headers
        .get(LOCALES_HEADER)
        .is_some_and(|value| value == "v1")
}

pub(crate) fn contains_extended(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields
                .get("locale")
                .and_then(Value::as_str)
                .and_then(crate::speech::locale::EnglishLocale::parse)
                .is_some_and(|locale| !locale.is_legacy())
                || fields.get("gender").and_then(Value::as_str) == Some("neutral")
                || fields.values().any(contains_extended)
        }
        Value::Array(items) => items.iter().any(contains_extended),
        _ => false,
    }
}

/// Filter only the response copy; never reinterpret AU/NZ/CA as GB/US.
pub(crate) fn project_extended(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("accent_group");
            fields.values_mut().for_each(project_extended);
        }
        Value::Array(items) => {
            items.retain(|item| {
                item.get("gender").and_then(Value::as_str) != Some("neutral")
                    && item
                        .get("locale")
                        .and_then(Value::as_str)
                        .and_then(crate::speech::locale::EnglishLocale::parse)
                        .is_none_or(|locale| locale.is_legacy())
            });
            items.iter_mut().for_each(project_extended);
        }
        _ => {}
    }
}

pub(crate) fn contains_audio(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            (fields.contains_key("voice_id") && fields.contains_key("audio_asset_id"))
                || fields.values().any(contains_audio)
        }
        Value::Array(items) => items.iter().any(contains_audio),
        _ => false,
    }
}

pub(crate) fn project_audio(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            if fields.contains_key("voice_id") {
                fields.remove("audio_asset_id");
            }
            fields.values_mut().for_each(project_audio);
        }
        Value::Array(items) => items.iter_mut().for_each(project_audio),
        _ => {}
    }
}

pub(crate) fn contains_versions(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            (fields.contains_key("voice_id") && fields.contains_key("version_id"))
                || fields.values().any(contains_versions)
        }
        Value::Array(items) => items.iter().any(contains_versions),
        _ => false,
    }
}

pub(crate) fn project(value: &mut Value) {
    if let Some(voices) = value.get_mut("voices").and_then(Value::as_array_mut)
        && voices.iter().any(|voice| voice.get("version_id").is_some())
    {
        let enabled = voices
            .iter()
            .filter(|voice| voice["enabled"] == true)
            .filter_map(|voice| voice["voice_id"].as_str().map(str::to_owned))
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        voices.retain_mut(|voice| {
            let Some(id) = voice["voice_id"].as_str().map(str::to_owned) else {
                return true;
            };
            if enabled.contains(&id) && voice["enabled"] != true {
                return false;
            }
            if !seen.insert(id) {
                return false;
            }
            if let Some(fields) = voice.as_object_mut() {
                fields.remove("version_id");
                fields.remove("version_name");
                fields.remove("locale");
                fields.remove("audio_asset_id");
            }
            true
        });
    }
    match value {
        Value::Object(fields) => fields.values_mut().for_each(project),
        Value::Array(items) => items.iter_mut().for_each(project),
        _ => {}
    }
}

pub(crate) fn word_write_error(
    id: uuid::Uuid,
    step: crate::lexicon::dto::PersistedWordStep,
) -> crate::error::AppError {
    use crate::lexicon::dto::{V3DraftNodeLocation, V3DraftValidationIssue, V3ValidationIssueCode};
    crate::error::AppError::unprocessable(
        crate::error::ErrorCode::ValidationFailed,
        UPGRADE_MESSAGE,
    )
    .with_v3_field_issues(vec![V3DraftValidationIssue {
        schema_version: 3,
        step,
        node_id: id,
        field: "voice_profile".into(),
        code: V3ValidationIssueCode::VoiceProfileInvalid,
        message: UPGRADE_MESSAGE.into(),
        node_location: V3DraftNodeLocation {
            content_id: None,
            content_slot_id: None,
            node_role: "entry".into(),
            ancestor_node_ids: vec![],
            pos_id: None,
            form_group_id: None,
            membership_id: None,
            form_id: None,
            variant_id: None,
            pronunciation_id: None,
            form_type: None,
            dialect: None,
        },
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_views_filter_extended_regions_without_reclassifying_or_mutating_storage() {
        let stored = json!({"voice_profile":{"voices":[
            {"voice_id":"sonia","locale":"en-GB","version_id":"a","enabled":false},
            {"voice_id":"natasha","locale":"en-AU","version_id":"b","enabled":true},
            {"voice_id":"swara","locale":"en-IN","version_id":"c","enabled":true}
        ]},"audio":[{"locale":"en-GB","accent_group":"uk"},{"locale":"en-IN","accent_group":"other"}]});
        assert!(contains_extended(&stored));
        let mut old = stored.clone();
        project_extended(&mut old);
        assert_eq!(old["voice_profile"]["voices"].as_array().unwrap().len(), 1);
        assert_eq!(old["audio"], json!([{"locale":"en-GB"}]));
        assert!(!contains_extended(&old));
        assert_eq!(stored["audio"].as_array().unwrap().len(), 2);
        for (version, versions, generated, extended) in [
            (None, false, false, false),
            (Some("v1"), true, false, false),
            (Some("v2"), true, true, false),
            (Some("v3"), true, true, false),
            (Some("v4"), false, false, false),
        ] {
            let mut headers = HeaderMap::new();
            if let Some(version) = version {
                headers.insert(HEADER, version.parse().unwrap());
            }
            assert_eq!(supported(&headers), versions);
            assert_eq!(generated_supported(&headers), generated);
            assert_eq!(extended_supported(&headers), extended);
        }
        let mut negotiated = HeaderMap::new();
        negotiated.insert(HEADER, "v2".parse().unwrap());
        negotiated.insert(LOCALES_HEADER, "v1".parse().unwrap());
        assert!(supported(&negotiated));
        assert!(generated_supported(&negotiated));
        assert!(extended_supported(&negotiated));
    }

    #[test]
    fn model_projection_and_guard_do_not_rewrite_normal_versions() {
        let stored = json!({"voices":[{"voice_id":"sonia","locale":"en-GB"},{"voice_id":"azure-hd-flash-id","locale":"en-US"}]});
        assert!(contains_models(&stored));
        let mut old = stored.clone();
        project_models(&mut old);
        assert_eq!(
            old["voices"],
            json!([{"voice_id":"sonia","locale":"en-GB"}])
        );
        assert!(!contains_models(&old));
        assert!(contains_models(&stored));
    }

    #[test]
    fn dubbing_projection_removes_new_semantics_and_preserves_stored_content() {
        let stored = json!({"voice_profile":{"voices":[{"voice_id":"sonia","enabled":true,"style_degree":1.2,"role":"Girl"}],"dubbing":{"mode":"local","area":"meaning"}},"reading_reference":{"uk":{}},"reading_links":[],"value":{"version":2,"text":"123","annotations":[{"type":"say_as","start":0,"end":3,"interpret_as":"digits"},{"type":"sub","start":0,"end":3,"alias":"one"},{"type":"italic","start":0,"end":3}]}});
        assert!(contains_dubbing(&stored));
        let mut legacy = stored.clone();
        project_dubbing(&mut legacy);
        assert!(!contains_dubbing(&legacy));
        assert_eq!(
            legacy["voice_profile"]["voices"][0],
            json!({"voice_id":"sonia","enabled":true})
        );
        assert_eq!(
            legacy["value"]["annotations"],
            json!([{"type":"italic","start":0,"end":3}])
        );
        assert!(stored["voice_profile"]["dubbing"].is_object());
    }

    #[test]
    fn spelling_recordings_require_new_client_without_hiding_existing_grammar_recordings() {
        let stored = json!({"spelling_speech":{"content":{"version":2,"text":"cat","annotations":[]},"voice_profile":{"voices":[]},"audio_assets":[{"id":"recording"}]},"grammar":{"content":{},"voice_profile":{"voices":[]},"audio_assets":[{"id":"grammar"}]}});
        assert!(contains_dubbing(&stored));
        let mut legacy = stored.clone();
        project_dubbing(&mut legacy);
        assert!(!contains_dubbing(&legacy));
        assert!(legacy["spelling_speech"].get("audio_assets").is_none());
        assert_eq!(
            legacy["grammar"]["audio_assets"],
            stored["grammar"]["audio_assets"]
        );
    }

    #[test]
    fn legacy_view_keeps_selected_parameters_without_changing_stored_versions() {
        let stored = json!({"voice_profile":{"voices":[
            {"voice_id":"sonia","enabled":false,"rate_percent":-25,"version_id":"a","version_name":"慢读","locale":"en-GB"},
            {"voice_id":"sonia","enabled":true,"rate_percent":0,"version_id":"b","version_name":"自然","locale":"en-GB"},
            {"voice_id":"retired","enabled":true,"rate_percent":10}
        ]}});
        let mut response = stored.clone();
        project(&mut response);
        assert_eq!(
            response["voice_profile"]["voices"],
            json!([
                {"voice_id":"sonia","enabled":true,"rate_percent":0},
                {"voice_id":"retired","enabled":true,"rate_percent":10}
            ])
        );
        assert!(contains_versions(&stored));
        assert!(!contains_versions(&response));
    }
}
