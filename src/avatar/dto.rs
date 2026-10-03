use crate::auth::handler::UserProfile;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
pub struct AvatarUploadRequest {
    pub content_type: String,
    #[schema(minimum = 1, maximum = 5242880)]
    pub size: u64,
}

#[derive(Deserialize, ToSchema)]
pub struct AvatarConfirmRequest {
    pub key: String,
}

#[derive(Serialize, ToSchema)]
pub struct AvatarUpload {
    pub key: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub expires_in: u64,
    pub max_bytes: u64,
}

#[derive(Serialize, ToSchema)]
pub struct AvatarUploadResponse {
    pub upload: AvatarUpload,
}

#[derive(Serialize, ToSchema)]
pub struct AvatarConfirmResponse {
    pub user: UserProfile,
}

#[derive(ToSchema)]
#[schema(value_type=String, format=Binary)]
pub struct AvatarImage(pub Vec<u8>);
