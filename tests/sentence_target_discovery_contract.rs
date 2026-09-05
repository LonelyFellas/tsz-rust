use serde_json::Value;
use tsz_rust::openapi::ApiDoc;
use utoipa::OpenApi;

const RESOLVE_PATH: &str = "/api/v1/admin/lexicon/entries/sentence-targets/resolve";

fn spec() -> Value {
    serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI should serialize")
}

fn operation<'a>(spec: &'a Value, path: &str, method: &str) -> &'a Value {
    spec.get("paths")
        .and_then(|paths| paths.get(path))
        .and_then(|path_item| path_item.get(method))
        .unwrap_or_else(|| panic!("missing {method} {path}"))
}

fn request_schema<'a>(spec: &'a Value, operation: &'a Value) -> &'a Value {
    let schema = operation
        .pointer("/requestBody/content/application~1json/schema")
        .expect("operation should expose an application/json request schema");
    dereference(spec, schema)
}

fn response_schema<'a>(spec: &'a Value, operation: &'a Value, status: &str) -> &'a Value {
    let pointer = format!("/responses/{status}/content/application~1json/schema");
    let schema = operation
        .pointer(&pointer)
        .unwrap_or_else(|| panic!("operation should expose an application/json {status} schema"));
    dereference(spec, schema)
}

fn dereference<'a>(spec: &'a Value, schema: &'a Value) -> &'a Value {
    let mut current = schema;
    let mut visited = std::collections::HashSet::new();
    while let Some(reference) = current.get("$ref").and_then(Value::as_str) {
        assert!(
            reference.starts_with("#/components/schemas/"),
            "unexpected schema ref: {reference}"
        );
        assert!(visited.insert(reference), "cyclic schema ref: {reference}");
        current = spec
            .pointer(reference.trim_start_matches('#'))
            .unwrap_or_else(|| panic!("unresolved schema ref: {reference}"));
    }
    current
}

fn schema_property<'a>(spec: &'a Value, schema: &'a Value, name: &str) -> &'a Value {
    let schema = dereference(spec, schema);
    let property = schema
        .get("properties")
        .and_then(|properties| properties.get(name))
        .unwrap_or_else(|| panic!("missing property {name}"));
    dereference(spec, property)
}

fn required_fields(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn assert_required(spec: &Value, schema: &Value, names: &[&str]) {
    let schema = dereference(spec, schema);
    let required = required_fields(schema);
    for name in names {
        assert!(required.contains(name), "{name} should be required");
    }
}

fn literal(spec: &Value, schema: &Value) -> Option<Value> {
    let schema = dereference(spec, schema);
    if let Some(value) = schema.get("const") {
        return Some(value.clone());
    }
    let values = schema.get("enum")?.as_array()?;
    (values.len() == 1).then(|| values[0].clone())
}

fn branch_by_literal<'a>(
    spec: &'a Value,
    root: &'a Value,
    property: &str,
    expected: &Value,
) -> &'a Value {
    dereference(spec, root)
        .get("oneOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|branch| dereference(spec, branch))
        .find(|branch| {
            branch
                .get("properties")
                .and_then(|properties| properties.get(property))
                .and_then(|schema| literal(spec, schema))
                .as_ref()
                == Some(expected)
        })
        .unwrap_or_else(|| panic!("missing {property}={expected} branch"))
}

fn component_by_required<'a>(spec: &'a Value, names: &[&str]) -> &'a Value {
    spec.pointer("/components/schemas")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|schemas| schemas.values())
        .map(|schema| dereference(spec, schema))
        .find(|schema| {
            let required = required_fields(schema);
            names.iter().all(|name| required.contains(name))
        })
        .unwrap_or_else(|| panic!("missing schema requiring {names:?}"))
}

#[test]
fn resolve_request_is_a_strict_mode_tagged_union() {
    let spec = spec();
    let operation = operation(&spec, RESOLVE_PATH, "post");
    let request = request_schema(&spec, operation);

    assert_eq!(
        request.pointer("/discriminator/propertyName"),
        Some(&Value::String("mode".to_owned()))
    );
    assert_eq!(
        request.get("oneOf").and_then(Value::as_array).map(Vec::len),
        Some(2)
    );

    let automatic = branch_by_literal(
        &spec,
        request,
        "mode",
        &Value::String("all_published_targets".to_owned()),
    );
    assert_required(
        &spec,
        automatic,
        &["schema_version", "sentence_text", "source_dialect", "mode"],
    );
    assert_eq!(
        automatic
            .pointer("/properties/schema_version")
            .and_then(|schema| literal(&spec, schema)),
        Some(Value::from(3))
    );
    assert!(automatic.pointer("/properties/selected_segments").is_none());
    assert!(automatic.pointer("/properties/include_drafts").is_none());

    let selected = branch_by_literal(
        &spec,
        request,
        "mode",
        &Value::String("selected_segments".to_owned()),
    );
    assert_required(
        &spec,
        selected,
        &[
            "schema_version",
            "sentence_text",
            "source_dialect",
            "mode",
            "selected_segments",
            "include_drafts",
        ],
    );
    let segments = schema_property(&spec, selected, "selected_segments");
    assert_eq!(segments.get("minItems").and_then(Value::as_u64), Some(1));
    assert_eq!(segments.get("maxItems").and_then(Value::as_u64), Some(20));
    assert!(schema_property(&spec, selected, "cursor").is_object());
}

#[test]
fn resolve_response_binds_completeness_generation_and_segment_ranges() {
    let spec = spec();
    let operation = operation(&spec, RESOLVE_PATH, "post");
    let response = response_schema(&spec, operation, "200");

    assert_required(
        &spec,
        response,
        &[
            "schema_version",
            "sentence_hash",
            "discovery_generation",
            "completeness",
            "range_results",
        ],
    );
    assert_eq!(
        response
            .pointer("/properties/schema_version")
            .and_then(|schema| literal(&spec, schema)),
        Some(Value::from(3))
    );
    assert_eq!(
        schema_property(&spec, response, "completeness").get("enum"),
        Some(&serde_json::json!(["complete", "overloaded"]))
    );

    let range = schema_property(&spec, response, "range_results")
        .get("items")
        .map(|schema| dereference(&spec, schema))
        .expect("range_results should expose item schema");
    assert_required(
        &spec,
        range,
        &[
            "source_segments",
            "segments_fingerprint",
            "published_total",
            "published_matches",
            "draft_matches",
        ],
    );
    assert!(range.pointer("/properties/source_range").is_none());
    let segments = schema_property(&spec, range, "source_segments");
    assert_eq!(segments.get("minItems").and_then(Value::as_u64), Some(1));
    assert_eq!(segments.get("maxItems").and_then(Value::as_u64), Some(20));
    assert!(schema_property(&spec, range, "next_cursor").is_object());
}

#[test]
fn resolve_candidates_preserve_base_and_sense_identity_and_draft_safety() {
    let spec = spec();
    operation(&spec, RESOLVE_PATH, "post");

    let base_candidate = component_by_required(
        &spec,
        &[
            "entry_id",
            "publication_id",
            "pos_id",
            "base_form_id",
            "matches",
            "senses",
        ],
    );
    assert_required(&spec, base_candidate, &["kind", "forms"]);
    assert_eq!(
        schema_property(&spec, base_candidate, "kind").get("enum"),
        Some(&serde_json::json!(["word", "phrase"]))
    );
    let candidate_form = schema_property(&spec, base_candidate, "forms")
        .get("items")
        .map(|schema| dereference(&spec, schema))
        .expect("candidate forms should expose item schema");
    assert_required(
        &spec,
        candidate_form,
        &[
            "form_id",
            "variant_id",
            "form_type",
            "spelling",
            "dialect",
            "base_form_ids",
        ],
    );
    // 前端 runtime contract 对这两层对象都是 fail-closed，后端加字段必须前端先同步；
    // 这里把封闭性和数组上限钉死，免得 deny_unknown_fields 或 max_items 被悄悄拿掉。
    for closed in [base_candidate, candidate_form] {
        assert_eq!(
            closed.get("additionalProperties"),
            Some(&Value::Bool(false)),
            "resolve candidates must stay closed objects"
        );
    }
    let forms = schema_property(&spec, base_candidate, "forms");
    assert_eq!(forms.get("type").and_then(Value::as_str), Some("array"));
    assert_eq!(forms.get("maxItems").and_then(Value::as_u64), Some(2000));
    let base_form_ids = schema_property(&spec, candidate_form, "base_form_ids");
    assert_eq!(
        base_form_ids.get("type").and_then(Value::as_str),
        Some("array")
    );
    assert_eq!(
        base_form_ids.get("maxItems").and_then(Value::as_u64),
        Some(2000)
    );
    assert_eq!(
        base_form_ids.pointer("/items/type").and_then(Value::as_str),
        Some("string")
    );
    assert_eq!(
        base_form_ids
            .pointer("/items/format")
            .and_then(Value::as_str),
        Some("uuid")
    );
    let sense = schema_property(&spec, base_candidate, "senses")
        .get("items")
        .map(|schema| dereference(&spec, schema))
        .expect("senses should expose item schema");
    assert_required(
        &spec,
        sense,
        &[
            "sense_id",
            "publication_id",
            "pos_id",
            "base_form_id",
            "level",
            "gloss",
        ],
    );

    let draft_candidate = component_by_required(
        &spec,
        &["entry_id", "entry_revision", "target_state", "linkability"],
    );
    assert_eq!(
        draft_candidate
            .pointer("/properties/target_state")
            .and_then(|schema| literal(&spec, schema)),
        Some(Value::String("draft".to_owned()))
    );
    assert_eq!(
        draft_candidate
            .pointer("/properties/linkability")
            .and_then(|schema| literal(&spec, schema)),
        Some(Value::String("pending_only".to_owned()))
    );
    assert!(
        draft_candidate
            .pointer("/properties/publication_id")
            .is_none()
    );
}
