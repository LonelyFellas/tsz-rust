use axum::http::HeaderMap;
use serde_json::Value;
use std::collections::HashSet;

pub(crate) const HEADER: &str = "x-tsz-voice-versions";
pub(crate) const UPGRADE_MESSAGE: &str =
    "此内容包含新版文本配音版本，请保留本地输入并刷新页面后再保存";

pub(crate) fn supported(headers: &HeaderMap) -> bool {
    headers
        .get(HEADER)
        .is_some_and(|value| value == "v1" || value == "v2")
}

pub(crate) fn generated_supported(headers: &HeaderMap) -> bool {
    headers.get(HEADER).is_some_and(|value| value == "v2")
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
