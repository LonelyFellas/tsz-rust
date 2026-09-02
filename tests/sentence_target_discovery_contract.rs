use serde_json::Value;
use tsz_rust::openapi::ApiDoc;
use utoipa::OpenApi;

const RESOLVE_PATH: &str = "/api/v1/admin/lexicon/entries/sentence-targets/resolve";
const REPLACE_PATH: &str =
    "/api/v1/admin/lexicon/entries/{id}/sentences/{sentence_id}/associations";

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

fn component_schema<'a>(spec: &'a Value, name: &str) -> &'a Value {
    spec.pointer(&format!("/components/schemas/{name}"))
        .unwrap_or_else(|| panic!("missing components.schemas.{name}"))
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
        &["form_id", "variant_id", "form_type", "spelling", "dialect"],
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

#[test]
fn association_v3_uses_segments_as_the_only_position_authority() {
    let spec = spec();
    let replace = operation(&spec, REPLACE_PATH, "put");
    let request = request_schema(&spec, replace);
    let v3 = branch_by_literal(
        &spec,
        request,
        "association_schema_version",
        &Value::from(3),
    );
    assert_required(&spec, v3, &["association_schema_version", "associations"]);
    let v3_item = schema_property(&spec, v3, "associations")
        .get("items")
        .map(|schema| dereference(&spec, schema))
        .expect("V3 associations should expose item schema");
    assert_required(&spec, v3_item, &["id", "source_dialect", "source_segments"]);
    assert!(v3_item.pointer("/properties/source_range").is_none());
    let segments = schema_property(&spec, v3_item, "source_segments");
    assert_eq!(segments.get("minItems").and_then(Value::as_u64), Some(1));
    assert_eq!(segments.get("maxItems").and_then(Value::as_u64), Some(20));

    let legacy = request
        .get("oneOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|branch| dereference(&spec, branch))
        .find(|branch| {
            branch
                .pointer("/properties/association_schema_version")
                .is_none()
        })
        .expect("legacy source_range branch should remain explicit");
    let legacy_item = schema_property(&spec, legacy, "associations")
        .get("items")
        .map(|schema| dereference(&spec, schema))
        .expect("legacy associations should expose item schema");
    assert_required(&spec, legacy_item, &["source_range"]);
    assert!(legacy_item.pointer("/properties/source_segments").is_none());

    for name in [
        "SentenceAssociationInputV3",
        "PendingSentenceAssociationItemV3",
    ] {
        let schema = component_schema(&spec, name);
        assert_required(&spec, schema, &["source_segments"]);
        assert!(
            schema.pointer("/properties/source_range").is_none(),
            "{name} must not mix source_range with source_segments"
        );
    }

    let projected = component_schema(&spec, "WordSentenceAssociationV3");
    for state in ["linked", "pending"] {
        let branch = branch_by_literal(&spec, projected, "state", &Value::String(state.to_owned()));
        assert_required(&spec, branch, &["source_segments"]);
        assert!(
            branch.pointer("/properties/source_range").is_none(),
            "WordSentenceAssociationV3 {state} branch must not mix source_range with source_segments"
        );
    }

    let v2 = component_schema(&spec, "SentenceAssociationInputV2");
    assert_required(&spec, v2, &["source_range"]);
    assert!(v2.pointer("/properties/source_segments").is_none());
}
