//! Compare writable content before normalization/target resolution mutates it.
use std::collections::BTreeMap;

use serde_json::Value;
use uuid::Uuid;

use super::AdminAuthorization;
use crate::error::AppError;

fn canonical_associations(value: &Value, text_links: bool) -> Value {
    let mut value = value.clone();
    if let Some(items) = value.as_array_mut() {
        if text_links {
            for item in items.iter_mut() {
                if let Some(link) = item.as_object_mut() {
                    link.remove("target_headword");
                    link.remove("target_gloss");
                }
            }
        }
        // Collection order is not a permission change; preserve each link's ordered segments.
        items.sort_by_cached_key(Value::to_string);
    }
    value
}

fn split(value: &Value, path: &str, field: &str, links: &mut BTreeMap<String, Value>) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter_map(|(key, value)| {
                    // Mirror the server-owned fields omitted by WordRelationWritableV3
                    // and WordSentenceWritableV3; their writable target/links remain content.
                    if (field == "relations"
                        && matches!(
                            key.as_str(),
                            "target_headword" | "target_gloss" | "target_status"
                        ))
                        || (field == "sentences"
                            && matches!(key.as_str(), "associations" | "associations_state"))
                    {
                        return None;
                    }
                    if matches!(key.as_str(), "text_links" | "form_links") {
                        if value.as_array().is_some_and(|items| !items.is_empty()) {
                            links.insert(
                                format!("{path}/{key}"),
                                canonical_associations(value, key == "text_links"),
                            );
                        }
                        None
                    } else {
                        Some((
                            key.clone(),
                            split(value, &format!("{path}/{key}"), key, links),
                        ))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let identity = item
                        .get("id")
                        .or_else(|| item.get("pos_id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| index.to_string());
                    split(item, &format!("{path}/{identity}"), field, links)
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

pub fn require_changes(
    authorization: &AdminAuthorization,
    module: &str,
    owner: Uuid,
    before: &Value,
    after: &Value,
) -> Result<(), AppError> {
    let edit = format!("{module}.edit");
    let associate = format!("{module}.associate");
    authorization.require_any_owned_action(&[&edit, &associate], owner)?;
    let mut previous_links = BTreeMap::new();
    let mut next_links = BTreeMap::new();
    let mut previous_content = before.clone();
    let mut next_content = after.clone();
    // Shared sentence annotations live beside the sentence, not inside rich text.
    if module == "sentences" {
        for (content, links) in [
            (&mut previous_content, &mut previous_links),
            (&mut next_content, &mut next_links),
        ] {
            if let Some(annotations) = content
                .as_object_mut()
                .and_then(|value| value.remove("annotations"))
            {
                links.insert(
                    "annotations".into(),
                    canonical_associations(&annotations, false),
                );
            }
        }
    }
    let previous_content = split(&previous_content, "", "", &mut previous_links);
    let next_content = split(&next_content, "", "", &mut next_links);
    if previous_content != next_content {
        authorization.require_owned_action(&edit, owner)?;
    }
    if previous_links != next_links {
        authorization.require_owned_action(&associate, owner)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn actor(module: &str, action: &str, others: bool) -> AdminAuthorization {
        let mut permissions = super::super::catalog::all_keys();
        permissions.retain(|key| key.ends_with(".access"));
        permissions.insert(format!("{module}.{action}"));
        if others {
            permissions.insert(format!("{module}.edit_others"));
        }
        AdminAuthorization {
            admin_id: Uuid::nil(),
            is_super_admin: false,
            permission_version: 0,
            permissions,
        }
    }

    #[test]
    fn association_only_rejects_text_audio_structure_and_mixed_updates() {
        let before = json!({"pos":[{"pos_id":"p", "variants":[{"id":"v", "value":{"text":"hello"}, "voice_profile":{"voices":[]}, "text_links":[]}]}]});
        let mut linked = before.clone();
        linked["pos"][0]["variants"][0]["text_links"] =
            json!([{"id":"l","target_word_id":"target"}]);
        let authorization = actor("words", "associate", false);
        assert!(require_changes(&authorization, "words", Uuid::nil(), &before, &linked).is_ok());
        for (key, value) in [
            ("value", json!({"text":"changed"})),
            ("voice_profile", json!({"voices":["new"]})),
            ("id", json!("other")),
        ] {
            let mut forged = linked.clone();
            forged["pos"][0]["variants"][0][key] = value;
            assert!(
                require_changes(&authorization, "words", Uuid::nil(), &before, &forged).is_err(),
                "{key}"
            );
        }
        assert!(
            require_changes(
                &actor("words", "edit", false),
                "words",
                Uuid::nil(),
                &before,
                &linked
            )
            .is_err()
        );
        let mut changed = before.clone();
        changed["pos"][0]["variants"][0]["value"]["text"] = json!("changed");
        assert!(
            require_changes(
                &actor("words", "edit", false),
                "words",
                Uuid::nil(),
                &before,
                &changed
            )
            .is_ok()
        );
    }

    #[test]
    fn writable_content_ignores_server_owned_link_and_relation_labels() {
        let before = json!({"pos":[{"pos_id":"p","grammar_structures":[],"senses":[{"id":"s","level":"A1","relations":[{"id":"r","relation":"synonym","target_word_id":"w","target_sense_id":"s2","score":"1","target_headword":"hello","target_gloss":"你好","target_status":"draft"}],"definitions":[{"id":"d","content":{"common":{"id":"v","value":{"text":"hello"},"text_links":[{"id":"l","target_word_id":"w","target_headword":"hello","target_gloss":"你好"}]}}}]}]}]});
        let mut writable = before.clone();
        let sense = &mut writable["pos"][0]["senses"][0];
        for key in ["target_headword", "target_gloss", "target_status"] {
            sense["relations"][0].as_object_mut().unwrap().remove(key);
        }
        for key in ["target_headword", "target_gloss"] {
            sense["definitions"][0]["content"]["common"]["text_links"][0]
                .as_object_mut()
                .unwrap()
                .remove(key);
        }
        let mut content_edit = writable.clone();
        content_edit["pos"][0]["senses"][0]["level"] = json!("B1");
        assert!(
            require_changes(
                &actor("words", "edit", false),
                "words",
                Uuid::nil(),
                &before,
                &content_edit
            )
            .is_ok()
        );
        let mut association_edit = writable.clone();
        association_edit["pos"][0]["senses"][0]["definitions"][0]["content"]["common"]["text_links"] =
            json!([]);
        assert!(
            require_changes(
                &actor("words", "associate", false),
                "words",
                Uuid::nil(),
                &before,
                &association_edit
            )
            .is_ok()
        );
        association_edit["pos"][0]["senses"][0]["relations"][0]["target_sense_id"] =
            json!("different");
        assert!(
            require_changes(
                &actor("words", "associate", false),
                "words",
                Uuid::nil(),
                &before,
                &association_edit
            )
            .is_err()
        );
        writable["pos"][0]["senses"][0]["definitions"][0]["content"]["common"]["text_links"][0]["target_word_id"] =
            json!("different");
        assert!(
            require_changes(
                &actor("words", "edit", false),
                "words",
                Uuid::nil(),
                &before,
                &writable
            )
            .is_err()
        );
    }

    #[test]
    fn association_order_does_not_require_permission_but_segments_remain_ordered() {
        let before = json!({"sentence":{"id":"s","level":"A1"},"annotations":[{"id":"uk","source_dialect":"uk","source_segments":[{"start":0,"end":1},{"start":3,"end":4}]},{"id":"us","source_dialect":"us","source_segments":[{"start":0,"end":1}]}]});
        let mut after = before.clone();
        after["sentence"]["level"] = json!("B1");
        after["annotations"].as_array_mut().unwrap().reverse();
        assert!(
            require_changes(
                &actor("sentences", "edit", false),
                "sentences",
                Uuid::nil(),
                &before,
                &after
            )
            .is_ok()
        );
        after["annotations"][1]["source_segments"]
            .as_array_mut()
            .unwrap()
            .reverse();
        assert!(
            require_changes(
                &actor("sentences", "edit", false),
                "sentences",
                Uuid::nil(),
                &before,
                &after
            )
            .is_err()
        );
    }

    #[test]
    fn both_modules_share_scope_without_granting_actions() {
        for module in ["words", "sentences"] {
            let before = if module == "sentences" {
                json!({"sentence":{"id":"s","text":"hello"},"annotations":[]})
            } else {
                json!({"id":"v", "text":"hello", "form_links":[]})
            };
            let mut linked = before.clone();
            linked[if module == "sentences" {
                "annotations"
            } else {
                "form_links"
            }] = json!([{"id":"l"}]);
            let other = Uuid::now_v7();
            assert!(
                require_changes(
                    &actor(module, "associate", false),
                    module,
                    other,
                    &before,
                    &linked
                )
                .is_err()
            );
            assert!(
                require_changes(
                    &actor(module, "associate", true),
                    module,
                    other,
                    &before,
                    &linked
                )
                .is_ok()
            );
            assert!(
                require_changes(
                    &actor(module, "edit", true),
                    module,
                    other,
                    &before,
                    &linked
                )
                .is_err()
            );
            assert!(
                require_changes(
                    &actor(module, "edit_others", true),
                    module,
                    other,
                    &before,
                    &before
                )
                .is_err()
            );
            assert!(
                require_changes(
                    &actor(module, "edit", false),
                    module,
                    Uuid::nil(),
                    &linked,
                    &before
                )
                .is_err()
            );
        }
    }
}
