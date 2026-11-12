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
    headers.get(HEADER).is_some_and(|value| {
        value == "v1"
            || value == "v2"
            || value == "v3"
            || value == "v4"
            || value == "v5"
            || value == "v6"
    })
}

pub(crate) fn generated_supported(headers: &HeaderMap) -> bool {
    headers.get(HEADER).is_some_and(|value| {
        value == "v2" || value == "v3" || value == "v4" || value == "v5" || value == "v6"
    })
}

pub(crate) fn dubbing_supported(headers: &HeaderMap) -> bool {
    headers
        .get(HEADER)
        .is_some_and(|value| value == "v3" || value == "v4" || value == "v5" || value == "v6")
}

pub(crate) fn pool_supported(headers: &HeaderMap) -> bool {
    headers.get(HEADER).is_some_and(|value| value == "v6")
}

pub(crate) fn multi_recommendations_supported(headers: &HeaderMap) -> bool {
    headers
        .get(HEADER)
        .is_some_and(|value| value == "v5" || value == "v6")
}

pub(crate) fn contains_multi_recommendations(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields
                .get("recommendation")
                .is_some_and(contains_multi_recommendations_source)
                || fields.values().any(contains_multi_recommendations)
        }
        Value::Array(items) => items.iter().any(contains_multi_recommendations),
        _ => false,
    }
}

pub(crate) fn project_multi_recommendations(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            if fields
                .get("recommendation")
                .is_some_and(contains_multi_recommendations_source)
            {
                fields.remove("recommendation");
            }
            fields.values_mut().for_each(project_multi_recommendations);
        }
        Value::Array(items) => items.iter_mut().for_each(project_multi_recommendations),
        _ => {}
    }
}

fn contains_multi_recommendations_source(source: &Value) -> bool {
    ["voice_ids", "contribution_ids"].iter().any(|key| {
        source
            .get(*key)
            .and_then(Value::as_array)
            .is_some_and(|items| items.len() > 2)
    })
}

/// Read projection only. Original provenance is retained; source identities are never invented.
pub(crate) fn project_stored_dubbing(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            if fields.contains_key("voices")
                && fields
                    .get("dubbing")
                    .is_some_and(|d| matches!(d["mode"].as_str(), Some("inherit" | "local")))
            {
                let original = fields["dubbing"].clone();
                let projected = serde_json::json!({"area":original["area"],
                    "uk":{"mode":original["mode"]},"us":{"mode":original["mode"]}});
                fields.entry("legacy_dubbing").or_insert(original);
                fields.insert("dubbing".into(), projected);
            }
            for (key, child) in fields {
                if key != "legacy_dubbing" {
                    project_stored_dubbing(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(project_stored_dubbing),
        _ => {}
    }
}

pub(crate) fn stored_dubbing(mut value: Value) -> Value {
    project_stored_dubbing(&mut value);
    value
}

/// Preserve trusted provenance across edits, reordering and copies within the same aggregate.
pub(crate) fn preserve_legacy_dubbing(
    before: &Value,
    after: &mut Value,
) -> Result<bool, crate::error::AppError> {
    use std::collections::HashMap;
    fn identity(value: &Value) -> Option<&str> {
        value
            .get("id")
            .or_else(|| value.get("pos_id"))
            .and_then(Value::as_str)
    }
    fn collect<'a>(
        value: &'a Value,
        nodes: &mut HashMap<String, &'a Value>,
        origins: &mut HashSet<String>,
    ) {
        match value {
            Value::Object(fields) => {
                // Media references have their own ID namespace and cannot replace an authoring owner.
                if let Some(id) = identity(value)
                    && ["voice_profile", "spelling_speech", "synthesis"]
                        .iter()
                        .any(|key| fields.contains_key(*key))
                {
                    nodes.insert(id.into(), value);
                }
                if fields.contains_key("voices")
                    && let Some(raw) = fields.get("legacy_dubbing")
                {
                    origins.insert(raw.to_string());
                }
                for (key, child) in fields {
                    if key != "legacy_dubbing" {
                        collect(child, nodes, origins);
                    }
                }
            }
            Value::Array(items) => {
                for child in items {
                    collect(child, nodes, origins);
                }
            }
            _ => {}
        }
    }
    fn visit(
        before: &Value,
        after: &mut Value,
        nodes: &HashMap<String, &Value>,
        origins: &HashSet<String>,
    ) -> Result<bool, crate::error::AppError> {
        let before = identity(after)
            .and_then(|id| nodes.get(id).copied())
            .unwrap_or(before);
        let mut changed = false;
        match after {
            Value::Object(fields) => {
                if fields.contains_key("voices") {
                    let original = before.get("legacy_dubbing");
                    if fields.get("legacy_dubbing").is_some_and(|raw| {
                        original
                            .map_or_else(|| !origins.contains(&raw.to_string()), |old| old != raw)
                    }) {
                        return Err(crate::error::AppError::conflict(
                            crate::error::ErrorCode::UnsupportedSchemaVersion,
                            Some("voice_profile"),
                            "旧推荐出处由服务端保留，不能修改或伪造",
                        ));
                    }
                    if let Some(raw) = original
                        && fields.get("legacy_dubbing") != Some(raw)
                    {
                        fields.insert("legacy_dubbing".into(), raw.clone());
                        changed = true;
                    }
                }
                for (key, child) in fields {
                    if key != "legacy_dubbing" {
                        changed |= visit(&before[key.as_str()], child, nodes, origins)?;
                    }
                }
            }
            Value::Array(items) => {
                let old = before.as_array();
                for (index, child) in items.iter_mut().enumerate() {
                    let previous = old.and_then(|items| {
                        if let Some(id) = identity(child) {
                            items.iter().find(|old| identity(old) == Some(id))
                        } else {
                            items.get(index)
                        }
                    });
                    changed |= visit(previous.unwrap_or(&Value::Null), child, nodes, origins)?;
                }
            }
            _ => {}
        }
        Ok(changed)
    }
    let mut nodes = HashMap::new();
    let mut origins = HashSet::new();
    collect(before, &mut nodes, &mut origins);
    visit(before, after, &nodes, &origins)
}

pub(crate) fn contains_pool(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.contains_key("legacy_dubbing")
                || (fields.contains_key("voice_id") && fields.contains_key("accent"))
                || (fields.contains_key("ipa")
                    && fields.contains_key("ups")
                    && fields.contains_key("source"))
                || fields
                    .get("dubbing")
                    .is_some_and(|value| value.get("uk").is_some() || value.get("us").is_some())
                || fields.values().any(contains_pool)
        }
        Value::Array(items) => items.iter().any(contains_pool),
        _ => false,
    }
}

pub(crate) fn project_pool(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("legacy_dubbing");
            if fields.contains_key("voice_id") {
                fields.remove("accent");
            }
            if fields.contains_key("ipa") && fields.contains_key("ups") {
                fields.remove("source");
            }

            if fields
                .get("dubbing")
                .is_some_and(|d| d.get("uk").is_some() || d.get("us").is_some())
            {
                fields.remove("dubbing");
            }
            fields.values_mut().for_each(project_pool);
        }
        Value::Array(items) => items.iter_mut().for_each(project_pool),
        _ => {}
    }
}

pub(crate) fn unsupported_dubbing(headers: &HeaderMap, value: &Value) -> bool {
    (!dubbing_supported(headers) && contains_dubbing(value))
        || (!pool_supported(headers) && contains_pool(value))
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
            (Some("v4"), true, true, false),
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
    fn pool_negotiation_projects_only_response_and_protects_v3_writers() {
        let stored = json!({"voice_profile":{"voices":[],"dubbing":{"area":"word","uk":{"mode":"local","recommendations":[{"group_id":"g","recommendation_id":"p","voice_id":"sonia"}]},"us":{"mode":"local"}}}});
        let mut old = HeaderMap::new();
        old.insert(HEADER, "v3".parse().unwrap());
        assert!(dubbing_supported(&old));
        assert!(!pool_supported(&old));
        assert!(unsupported_dubbing(&old, &stored));
        let mut response = stored.clone();
        project_pool(&mut response);
        assert!(!contains_pool(&response));
        assert!(contains_pool(&stored));
        assert_eq!(response["voice_profile"]["dubbing"], Value::Null);
        old.insert(HEADER, "v6".parse().unwrap());
        assert!(pool_supported(&old));
        assert!(!unsupported_dubbing(&old, &stored));
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
        let stored = json!({"voice_profile":{"voices":[{"voice_id":"sonia","enabled":true,"style_degree":1.2,"role":"Girl"}],"dubbing":{"area":"meaning","uk":{"mode":"local"},"us":{"mode":"local"}}},"reading_reference":{"uk":{}},"reading_links":[],"value":{"version":2,"text":"123","annotations":[{"type":"say_as","start":0,"end":3,"interpret_as":"digits"},{"type":"sub","start":0,"end":3,"alias":"one"},{"type":"italic","start":0,"end":3}]}});
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

#[cfg(test)]
mod side_compatibility_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn v5_remains_legacy_and_v6_is_required_for_sides_and_provenance() {
        let mut headers = HeaderMap::new();
        headers.insert(HEADER, "v5".parse().unwrap());
        assert!(supported(&headers));
        assert!(multi_recommendations_supported(&headers));
        assert!(!pool_supported(&headers));
        let stored = json!({"voice_profile":{"voices":[{"voice_id":"sonia","enabled":true,"rate_percent":-27,"version_id":"stable","audio_asset_id":"owned"}],
            "dubbing":{"mode":"local","area":"word","pool_version":"explicit_v1","recommendation":{"group_id":"g","pair_id":"original","voice_ids":["sonia"],"contribution_ids":["stable"]}}}});
        let mut projected = stored.clone();
        project_stored_dubbing(&mut projected);
        assert_eq!(
            projected["voice_profile"]["voices"],
            stored["voice_profile"]["voices"]
        );
        assert_eq!(
            projected["voice_profile"]["legacy_dubbing"],
            stored["voice_profile"]["dubbing"]
        );
        assert_eq!(
            projected["voice_profile"]["dubbing"],
            json!({"area":"word","uk":{"mode":"local"},"us":{"mode":"local"}})
        );
        assert!(unsupported_dubbing(&headers, &projected));
        let mut legacy_wire = projected.clone();
        project_pool(&mut legacy_wire);
        assert!(legacy_wire["voice_profile"].get("legacy_dubbing").is_none());
        assert!(legacy_wire["voice_profile"].get("dubbing").is_none());
        assert_eq!(
            legacy_wire["voice_profile"]["voices"],
            stored["voice_profile"]["voices"]
        );
        headers.insert(HEADER, "v6".parse().unwrap());
        assert!(pool_supported(&headers));
        assert!(!unsupported_dubbing(&headers, &projected));
        let snapshot = projected.clone();
        project_stored_dubbing(&mut projected);
        assert_eq!(snapshot, projected, "read projection is idempotent");
    }

    #[test]
    fn provenance_survives_reordering_and_omission_but_cannot_be_forged() {
        let profile = |name| json!({"voices":[],"dubbing":{"mode":"local","area":"word","recommendation":{"pair_id":name}}});
        let mut before = json!([{"id":"a","voice_profile":profile("a")},{"id":"b","voice_profile":profile("b")}]);
        before[0]["audio_assets"] = json!([{"id":"a","locale":"en-GB"}]);
        project_stored_dubbing(&mut before);
        let mut after = json!([before[1].clone(), before[0].clone()]);
        for item in after.as_array_mut().unwrap() {
            item["voice_profile"]
                .as_object_mut()
                .unwrap()
                .remove("legacy_dubbing");
        }
        preserve_legacy_dubbing(&before, &mut after).unwrap();
        assert_eq!(
            after[0]["voice_profile"]["legacy_dubbing"],
            before[1]["voice_profile"]["legacy_dubbing"]
        );
        assert_eq!(
            after[1]["voice_profile"]["legacy_dubbing"],
            before[0]["voice_profile"]["legacy_dubbing"],
            "asset IDs are not voice configuration owner IDs"
        );
        let mut moved = json!({"uk":before[0].clone(),"us":before[1].clone()});
        moved["uk"]["voice_profile"]
            .as_object_mut()
            .unwrap()
            .remove("legacy_dubbing");
        preserve_legacy_dubbing(&before, &mut moved).unwrap();
        assert_eq!(
            moved["uk"]["voice_profile"]["legacy_dubbing"],
            before[0]["voice_profile"]["legacy_dubbing"]
        );
        let mut copied = json!([before[0].clone()]);
        copied[0]["id"] = json!("new-copy");
        preserve_legacy_dubbing(&before, &mut copied).unwrap();
        after[0]["voice_profile"]["legacy_dubbing"]["mode"] = json!("inherit");
        assert!(preserve_legacy_dubbing(&before, &mut after).is_err());
        assert!(preserve_legacy_dubbing(&Value::Null, &mut before).is_err());
    }
}
