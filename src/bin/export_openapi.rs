use std::{fs, path::Path};

use tsz_rust::openapi::ApiDoc;
use utoipa::OpenApi;

fn main() {
    let output = Path::new("docs/openapi.json");
    let json = serde_json::to_string(&ApiDoc::openapi()).expect("OpenAPI spec should serialize");
    fs::write(output, json).expect("OpenAPI spec should be written");
}
