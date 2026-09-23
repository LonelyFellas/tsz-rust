use serde_json::Value;
use tsz_rust::openapi::ApiDoc;
use utoipa::OpenApi;

fn spec() -> Value {
    serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI should serialize")
}

fn schema<'a>(spec: &'a Value, value: &'a Value) -> &'a Value {
    value
        .get("$ref")
        .and_then(Value::as_str)
        .map(|reference| &spec["components"]["schemas"][reference.rsplit('/').next().unwrap()])
        .unwrap_or(value)
}

#[test]
fn removed_sentence_discovery_is_not_exposed() {
    let spec = spec();
    assert!(
        spec["paths"]
            .get("/api/v1/admin/lexicon/entries/sentence-targets/resolve")
            .is_none()
    );
    for removed in [
        "ResolveSentenceTargetsV3Input",
        "ResolveSentenceTargetsV3Response",
        "SentenceTargetRangeResultV3",
        "SentenceTargetDiscoveryCompletenessV3",
    ] {
        assert!(
            spec["components"]["schemas"].get(removed).is_none(),
            "{removed}"
        );
    }
    assert!(
        spec["components"]["schemas"]["AdminWordV3Capabilities"]["properties"]
            .get("sentence_target_discovery")
            .is_none()
    );
}

#[test]
fn voice_editor_target_search_keeps_full_node_identity() {
    let spec = spec();
    assert!(
        spec["paths"]["/api/v1/admin/lexicon/entries/component-targets/search"]
            .get("post")
            .is_some()
    );
    let candidate = &spec["components"]["schemas"]["PublishedSentenceTargetCandidateV3"];
    for field in [
        "entry_id",
        "pos_id",
        "base_form_id",
        "matched_form_id",
        "matched_variant_id",
        "forms",
        "senses",
    ] {
        assert!(
            candidate["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == field),
            "{field}"
        );
    }
    assert_eq!(candidate["additionalProperties"], false);
    assert!(
        !candidate["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "publication_id")
    );
    let form = schema(&spec, &candidate["properties"]["forms"]["items"]);
    for field in [
        "form_id",
        "variant_id",
        "form_type",
        "spelling",
        "dialect",
        "base_form_ids",
    ] {
        assert!(
            form["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == field),
            "{field}"
        );
    }
    assert_eq!(form["additionalProperties"], false);
    assert_eq!(candidate["properties"]["forms"]["maxItems"], 2000);
    assert_eq!(form["properties"]["base_form_ids"]["maxItems"], 2000);
    let sense = schema(&spec, &candidate["properties"]["senses"]["items"]);
    for field in ["sense_id", "pos_id", "base_form_id", "level", "gloss"] {
        assert!(
            sense["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == field),
            "{field}"
        );
    }
    assert!(
        !sense["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "publication_id")
    );
}
