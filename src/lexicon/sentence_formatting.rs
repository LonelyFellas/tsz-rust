use axum::{
    Json,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    error::{AppError, ErrorCode},
    lexicon::dto::{
        PersistedWordStep, V3DraftNodeLocation, V3DraftValidationIssue, V3ValidationIssueCode,
    },
};

pub(crate) const HEADER: &str = "x-tsz-sentence-formatting";
pub(crate) const UPGRADE_MESSAGE: &str =
    "此内容包含新版加粗或下划线格式，请保留本地输入并刷新页面后再保存";

pub(crate) fn supported(headers: &HeaderMap) -> bool {
    headers.get(HEADER).is_some_and(|value| value == "v1")
}

fn new_annotation(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("bold" | "underline")
    )
}

fn rich_annotations(value: &Value) -> Option<&Vec<Value>> {
    (value.get("version").and_then(Value::as_u64) == Some(2)
        && value.get("text").is_some_and(Value::is_string))
    .then(|| value.get("annotations").and_then(Value::as_array))
    .flatten()
}

pub(crate) fn contains_new_formats(value: &Value) -> bool {
    if rich_annotations(value).is_some_and(|items| items.iter().any(new_annotation)) {
        return true;
    }
    match value {
        Value::Object(fields) => fields.values().any(contains_new_formats),
        Value::Array(items) => items.iter().any(contains_new_formats),
        _ => false,
    }
}

/// Traverse only the serialized response copy, including publication and wordlist wrappers.
fn project(value: &mut Value) {
    if rich_annotations(value).is_some()
        && let Some(items) = value.get_mut("annotations").and_then(Value::as_array_mut)
    {
        items.retain(|annotation| !new_annotation(annotation));
    }
    match value {
        Value::Object(fields) => fields.values_mut().for_each(project),
        Value::Array(items) => items.iter_mut().for_each(project),
        _ => {}
    }
}

pub(crate) fn response<T: Serialize>(
    headers: &HeaderMap,
    status: StatusCode,
    body: T,
) -> Result<Response, AppError> {
    if supported(headers) {
        return Ok((status, [(header::VARY, HEADER)], Json(body)).into_response());
    }
    let mut body = serde_json::to_value(body).map_err(AppError::internal)?;
    project(&mut body);
    Ok((status, [(header::VARY, HEADER)], Json(body)).into_response())
}

/// Existing validation vocabulary lets an already-running V3 editor show its refresh hint.
pub(crate) fn word_write_error(id: Uuid, step: PersistedWordStep) -> AppError {
    AppError::unprocessable(ErrorCode::ValidationFailed, UPGRADE_MESSAGE).with_v3_field_issues(
        vec![V3DraftValidationIssue {
            schema_version: 3,
            step,
            node_id: id,
            field: "sentence_formatting".into(),
            code: V3ValidationIssueCode::MeaningsStorageUnsafe,
            message: UPGRADE_MESSAGE.into(),
            node_location: V3DraftNodeLocation {
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
        }],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projection_removes_only_new_v2_formats_from_nested_response_copies() {
        let rich = json!({"version":2,"text":"😀 jobs","annotations":[
            {"type":"bold","start":2,"end":6},
            {"type":"underline","start":2,"end":6},
            {"type":"italic","start":2,"end":6},
            {"type":"liaison","start":0,"end":2},
            {"type":"pause","at":1,"duration_ms":250}
        ]});
        let stored = json!({"words":[{"meanings":{"common":rich}}],"sentences":[{"snapshot":{"sentence":{"uk":rich,"us":rich}}}],
            "legacy":{"version":1,"text":"jobs","spans":[{"type":"bold","start":0,"end":4}],"liaisons":[]},
            "unrelated":{"annotations":[{"type":"bold"}]}});
        let mut old = stored.clone();
        project(&mut old);
        assert!(!contains_new_formats(&old));
        assert!(contains_new_formats(&stored));
        assert_eq!(
            old["words"][0]["meanings"]["common"]["annotations"],
            json!([
                {"type":"italic","start":2,"end":6},{"type":"liaison","start":0,"end":2},{"type":"pause","at":1,"duration_ms":250}
            ])
        );
        assert_eq!(old["legacy"], stored["legacy"]);
        assert_eq!(old["unrelated"], stored["unrelated"]);
        for value in [None, Some("unknown"), Some("v1")] {
            let mut headers = HeaderMap::new();
            if let Some(value) = value {
                headers.insert(HEADER, value.parse().unwrap());
            }
            assert_eq!(supported(&headers), value == Some("v1"));
        }
    }
}
