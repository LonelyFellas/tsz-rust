use crate::otp::model::{Channel, Purpose};

/// 内部联调使用的统一验证码。只对 [`OtpSender::Mock`] 生效；真实短信通道不得复用。
const MOCK_CODE: &str = "000000";

/// Mock 通道仅允许在 debug 构建中使用。release 构建必须接入真实 sender，
/// 否则固定码会变成任意账号的登录/改密/注册凭证。
const MOCK_ALLOWED: bool = cfg!(debug_assertions);

pub enum OtpSender {
    /// 当前开发/测试环境 sender；仅在 debug 构建中使用固定验证码 000000。
    Mock,
    // Aliyun(AliyunSender),
}

#[derive(Debug, thiserror::Error)]
pub enum OtpSenderError {
    #[error("OTP delivery provider is unavailable")]
    Unavailable,
    /// release 构建没有配置真实 provider。启动即失败，避免固定码上线。
    #[error("no OTP delivery provider is configured for a release build")]
    NotConfigured,
    #[error(transparent)]
    Unknown(#[from] anyhow::Error),
    // Aliyun(#[from] aliyun_sdk::Error),
}

impl OtpSender {
    pub fn is_mock(&self) -> bool {
        matches!(self, OtpSender::Mock)
    }

    /// 启动时的 fail-closed 检查：release 构建禁止 Mock 通道。
    pub fn ensure_release_ready(&self) -> Result<(), OtpSenderError> {
        if self.is_mock() && !MOCK_ALLOWED {
            return Err(OtpSenderError::NotConfigured);
        }
        Ok(())
    }

    /// Mock 通道使用固定码，方便 debug 联调；真实通道返回 `None` 并走 CSPRNG。
    pub(crate) fn fixed_code(&self) -> Option<&'static str> {
        match self {
            OtpSender::Mock if MOCK_ALLOWED => Some(MOCK_CODE),
            OtpSender::Mock => None,
            // OtpSender::Aliyun(_) => None,
        }
    }

    pub(crate) fn ensure_available_for(&self, _purpose: Purpose) -> Result<(), OtpSenderError> {
        match self {
            OtpSender::Mock if MOCK_ALLOWED => Ok(()),
            OtpSender::Mock => Err(OtpSenderError::NotConfigured),
        }
    }

    pub fn send(&self, channel: Channel, _target: &str, _code: &str) -> Result<(), OtpSenderError> {
        match self {
            OtpSender::Mock if MOCK_ALLOWED => {
                tracing::info!(mock = true, ?channel, "otp_code_sent");
                Ok(())
            }
            OtpSender::Mock => Err(OtpSenderError::NotConfigured),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_sender_requires_a_debug_build() {
        assert_eq!(
            OtpSender::Mock.fixed_code().is_some(),
            cfg!(debug_assertions)
        );
        assert_eq!(
            OtpSender::Mock.ensure_available_for(Purpose::Login).is_ok(),
            cfg!(debug_assertions)
        );
        assert_eq!(
            OtpSender::Mock.ensure_release_ready().is_ok(),
            cfg!(debug_assertions)
        );
    }
}
