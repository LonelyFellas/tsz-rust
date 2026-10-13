//! Opt-in response fields keep existing strict clients compatible.
use crate::{error::AppError, lexicon::sentence_formatting};
use axum::{
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Serialize;

pub fn response<T: Serialize>(
    headers: &HeaderMap,
    status: StatusCode,
    body: T,
) -> Result<Response, AppError> {
    if headers
        .get("X-TSZ-Wordlist-Annotations")
        .is_some_and(|value| value == "v1")
    {
        return sentence_formatting::response(headers, status, body);
    }
    let mut body = serde_json::to_value(body).map_err(AppError::internal)?;
    if let Some(items) = body
        .get_mut("items")
        .and_then(serde_json::Value::as_array_mut)
    {
        for item in items {
            if let Some(item) = item.as_object_mut() {
                item.remove("author_note");
            }
        }
    }
    sentence_formatting::response(headers, status, body)
}
