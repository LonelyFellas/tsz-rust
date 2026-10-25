use axum::{
    Json,
    http::{HeaderValue, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
};
use serde::Serialize;

pub mod admin_handler;
mod annotations;
pub mod custom_audio;
pub mod deletion;
pub mod dto;
pub mod handler;
pub mod interactions;
pub mod library;
pub mod management;
mod notifications;
mod projection;
pub mod review;
pub mod service;
pub mod suggestions;
pub mod tips;

fn private_json(value: impl Serialize) -> Response {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response
}
