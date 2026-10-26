//! Opt-in response fields keep existing strict clients compatible.
use crate::{error::AppError, lexicon::sentence_formatting};
use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};
use serde::Serialize;

pub fn response<T: Serialize>(
    headers: &HeaderMap,
    status: StatusCode,
    body: T,
) -> Result<Response, AppError> {
    let mut body = serde_json::to_value(body).map_err(AppError::internal)?;
    let supports_custom = headers
        .get("X-TSZ-Wordlist-Custom-Items")
        .is_some_and(|v| v == "v1");
    if !supports_custom {
        let has_custom = body
            .get("custom_count")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
            > 0
            || body
                .get("items")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|i| i.get("custom_word").is_some_and(|v| !v.is_null()))
                });
        if has_custom {
            return Err(crate::error::AppError::conflict(
                crate::error::ErrorCode::WordListConflict,
                None,
                "此词表包含自定义词条，请刷新页面后重试",
            ));
        }
        if body.get("entry_ids").is_some() {
            body.as_object_mut().unwrap().remove("items");
        }
        if let Some(object) = body.as_object_mut() {
            object.remove("custom_count");
        }
    }
    let is_edit = body.get("entry_ids").is_some();
    let annotations = is_edit
        || headers
            .get("X-TSZ-Wordlist-Annotations")
            .is_some_and(|v| v == "v1");
    if let Some(items) = body
        .get_mut("items")
        .and_then(serde_json::Value::as_array_mut)
    {
        for item in items {
            if let Some(item) = item.as_object_mut() {
                if !annotations {
                    item.remove("author_note");
                }
                if !supports_custom {
                    item.remove("item_id");
                    item.remove("custom_word");
                }
            }
        }
    }
    let mut response = sentence_formatting::response(headers, status, body)?;
    response.headers_mut().insert(
        header::VARY,
        HeaderValue::from_static(
            "x-tsz-sentence-formatting, x-tsz-wordlist-annotations, x-tsz-wordlist-custom-items",
        ),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use serde_json::{Value, json};

    #[tokio::test]
    async fn annotation_negotiation_always_varies_on_both_reading_headers() {
        for version in [None, Some("v2"), Some("v1")] {
            let mut headers = HeaderMap::new();
            if let Some(version) = version {
                headers.insert(
                    "X-TSZ-Wordlist-Annotations",
                    HeaderValue::from_static(version),
                );
            }
            let result = response(
                &headers,
                StatusCode::OK,
                json!({"items":[{"author_note":"说明"}]}),
            )
            .unwrap();
            assert_eq!(
                result.headers()[header::VARY],
                "x-tsz-sentence-formatting, x-tsz-wordlist-annotations, x-tsz-wordlist-custom-items"
            );
            let body = result.into_body().collect().await.unwrap().to_bytes();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                body["items"][0].get("author_note").is_some(),
                version == Some("v1")
            );
        }
    }
}
