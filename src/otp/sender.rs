use crate::otp::model::Channel;

pub enum OtpSender {
    Mock,
    // Aliyun(AliyunSender),
}

#[derive(Debug, thiserror::Error)]
pub enum OtpSenderError {
    #[error(transparent)]
    Unknown(#[from] anyhow::Error),
    // Aliyun(#[from] aliyun_sdk::Error),
}

impl OtpSender {
    pub fn send(&self, channel: Channel, target: &str, code: &str) -> Result<(), OtpSenderError> {
        match self {
            OtpSender::Mock => {
                tracing::info!(mock = true, ?channel, target, code, "otp_code_sent");
                Ok(())
            }
        }
    }
}
