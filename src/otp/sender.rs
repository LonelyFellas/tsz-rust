use crate::otp::model::Channel;

/// 内部联调使用的统一验证码。只对 [`OtpSender::Mock`] 生效；真实短信通道不得复用。
const MOCK_CODE: &str = "000000";

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
    /// Mock 通道使用固定码，方便测试环境直接联调；真实通道返回 `None` 并走 CSPRNG。
    pub(crate) fn fixed_code(&self) -> Option<&'static str> {
        match self {
            OtpSender::Mock => Some(MOCK_CODE),
            // OtpSender::Aliyun(_) => None,
        }
    }

    pub fn send(&self, channel: Channel, target: &str, code: &str) -> Result<(), OtpSenderError> {
        match self {
            OtpSender::Mock => {
                tracing::info!(mock = true, ?channel, target, code, "otp_code_sent");
                Ok(())
            }
        }
    }
}
