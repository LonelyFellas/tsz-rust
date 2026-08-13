use serde::Deserialize;
use utoipa::ToSchema;

use crate::otp::model::Purpose;

pub mod handler;
pub mod model;
pub mod sender;
pub mod service;
pub mod store;
pub const OTP_MOUNT: &str = "/api/v1/otp";

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicOtpPurpose {
    Login,
    Register,
    PasswordReset,
    ContactBind,
}

impl From<PublicOtpPurpose> for Purpose {
    fn from(purpose: PublicOtpPurpose) -> Self {
        match purpose {
            PublicOtpPurpose::Login => Purpose::Login,
            PublicOtpPurpose::Register => Purpose::Register,
            PublicOtpPurpose::PasswordReset => Purpose::PasswordReset,
            PublicOtpPurpose::ContactBind => Purpose::ContactBind,
        }
    }
}
