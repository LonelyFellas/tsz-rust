use std::time::Duration;

use crate::otp::{
    model::{Channel, Purpose},
    sender::{OtpSender, OtpSenderError},
    store::{OtpStore, OtpStoreError},
};

#[derive(Debug, thiserror::Error)]
pub enum OtpServiceError {
    #[error("rate limit")]
    RateLimited,
    #[error("invalid code")]
    InvalidCode,
    #[error(transparent)]
    Store(#[from] OtpStoreError),
    #[error(transparent)]
    Send(#[from] OtpSenderError),
}

pub struct OtpService {
    store: OtpStore,
    cooldown: Duration,
    daily_limit: u64,
    sender: OtpSender,
    ttl: Duration,
    max_attempts: u8,
}

/// 日限窗口固定 24h（滚动）。
const DAILY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
impl OtpService {
    pub fn new(
        store: OtpStore,
        sender: OtpSender,
        cooldown: Duration,
        daily_limit: u64,
        ttl: Duration,
        max_attempts: u8,
    ) -> Self {
        Self {
            store,
            cooldown,
            daily_limit,
            sender,
            ttl,
            max_attempts,
        }
    }

    pub async fn request(&self, target: &str, purpose: Purpose) -> Result<(), OtpServiceError> {
        // 先 fail-close，避免未配置真实 provider 时写入无人可收取的验证码或消耗限额。
        self.sender.ensure_available_for(purpose)?;

        // 1) 冷却（0=）--最便宜，先查，挡掉多数连接点
        if !self.cooldown.is_zero()
            && !self
                .store
                .check_cooldown(target, purpose, self.cooldown)
                .await?
        {
            return Err(OtpServiceError::RateLimited);
        }

        // 2) 日限（0=关闭）
        if self.daily_limit > 0
            && !self
                .store
                .check_daily_limit(target, purpose, self.daily_limit, DAILY_WINDOW)
                .await?
        {
            return Err(OtpServiceError::RateLimited);
        }

        // 3) Mock 通道固定为 000000，方便内部联调；真实短信通道仍生成 6 位 CSPRNG 码。
        let code = self
            .sender
            .fixed_code()
            .map_or_else(generate_code, ToOwned::to_owned);

        // 4) 先存后发（send 失败无害：码自然过期，重试受冷却约束——otp-design §6）
        self.store
            .save_code(target, purpose, &code, self.ttl)
            .await?;

        // 5) 发
        self.sender
            .send(Channel::for_target(target), target, &code)?;
        Ok(())
    }

    pub async fn verify(
        &self,
        target: &str,
        purpose: Purpose,
        code: &str,
    ) -> Result<(), OtpServiceError> {
        if self
            .store
            .verify_code(target, purpose, code, self.max_attempts)
            .await?
        {
            Ok(())
        } else {
            Err(OtpServiceError::InvalidCode) // 错/过期/已用/没发过/锁死 → 统一
        }
    }
}
/// 6 位数字 CSPRNG 码。拒绝采样消除取模偏置，零填充到 6 位。
fn generate_code() -> String {
    const N: u32 = 1_000_000;
    let zone = (u32::MAX / N) * N; // 最大的 N 的倍数；>= zone 的样本拒绝、重采
    loop {
        let mut b = [0u8; 4];
        getrandom::fill(&mut b).expect("getrandom 应可用");
        let x = u32::from_le_bytes(b);
        if x < zone {
            return format!("{:06}", x % N); // {:06} = 零填充到 6 位
        }
    }
}
