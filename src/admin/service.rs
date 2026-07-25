use chrono::{Duration, Utc};
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    admin::{
        Admin, AdminRepository, AdminRepositoryError, AdminRole, AdminStatus, NewAdmin,
        model::SeedOutcome,
    },
    otp::{
        model::Purpose,
        service::{OtpService, OtpServiceError},
    },
    platform::{Password, PasswordError, Phone, PhoneError, dummy_hash},
};

pub const FAILED_LOGIN_THRESHOLD: i32 = 5;
pub const FAILED_LOGIN_LOCK_DURATION: Duration = Duration::seconds(60 * 15);

#[derive(Debug, thiserror::Error)]
pub enum AdminLoginError {
    #[error(transparent)]
    Phone(#[from] PhoneError),
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("account is disabled")]
    AccountDisabled,
    #[error("account temporarily locked")]
    Locked,
    #[error(transparent)]
    OtpUnavailable(#[from] OtpServiceError),
    #[error(transparent)]
    Repository(#[from] AdminRepositoryError),
}

#[derive(Debug, thiserror::Error)]
pub enum AdminSeedError {
    #[error(transparent)]
    Phone(#[from] PhoneError),
    // 透传策略/哈希错误——seed 是 CLI 场景，操作者需要知道具体哪条规则没过
    #[error(transparent)]
    Password(#[from] PasswordError),
    // 手机号已被一个普通 admin 占用：seed 只创建超管、绝不擅自提级已存在账号
    // （否则打错一位手机号就会把某个普通 admin 悄悄提成超管）。运维需手动处理冲突。
    #[error("phone already belongs to a non-super admin")]
    PhoneTakenByNonSuperAdmin,
    #[error(transparent)]
    Repository(#[from] AdminRepositoryError),
}

pub struct AdminService {
    repository: AdminRepository,
    /// `None` 仅用于 seed/CLI（不走 login）；登录路径必须注入。
    otp_service: Option<Arc<OtpService>>,
}

impl AdminService {
    pub fn new(repository: AdminRepository, otp_service: Arc<OtpService>) -> Self {
        Self {
            repository,
            otp_service: Some(otp_service),
        }
    }

    /// seed/CLI：不涉及 OTP 校验。
    pub fn for_seed(repository: AdminRepository) -> Self {
        Self {
            repository,
            otp_service: None,
        }
    }
    pub async fn login(
        &self,
        phone: &str,
        password: &str,
        code: &str,
    ) -> Result<Admin, AdminLoginError> {
        let phone = Phone::parse(phone).map_err(AdminLoginError::Phone)?;

        let admin = match self.repository.get_by_phone(phone.as_str()).await {
            Ok(admin) => admin,
            Err(AdminRepositoryError::NotFound) => {
                // 账号不存在，假设是密码错误, 安全起见，不返回具体错误信息。
                // 跑一次假 verify 平衡时序（不透明验哈希，不校验密码策略）。
                Password::verify_raw(password.to_string(), dummy_hash().to_string()).await;
                return Err(AdminLoginError::InvalidCredentials);
            }
            Err(e) => return Err(e.into()),
        };

        // 1) 检查账号是否正常
        if admin.is_locked(Utc::now()) {
            return Err(AdminLoginError::Locked);
        }

        // 2) 第二步 检查密码是否正确（不透明验哈希，不跑注册策略）
        if !Password::verify_raw(password.to_string(), admin.password_hash.clone()).await {
            // 2.1) 密码错误，进行累计
            self.repository
                .register_failed_login(
                    &admin.id,
                    FAILED_LOGIN_THRESHOLD,
                    FAILED_LOGIN_LOCK_DURATION,
                )
                .await?;
            return Err(AdminLoginError::InvalidCredentials);
        }

        // 3) 检查账号是否激活
        if admin.status == AdminStatus::Disabled {
            return Err(AdminLoginError::AccountDisabled);
        }

        // 4) 第四步 验证码验证
        let otp_service = self
            .otp_service
            .as_ref()
            .expect("AdminService::login 需要 otp_service");
        match otp_service
            .verify(phone.as_str(), Purpose::AdminLogin, code)
            .await
        {
            Ok(_) => (),
            Err(OtpServiceError::InvalidCode) => {
                // 4.1) 验证码错误, 进行累计
                self.repository
                    .register_failed_login(
                        &admin.id,
                        FAILED_LOGIN_THRESHOLD,
                        FAILED_LOGIN_LOCK_DURATION,
                    )
                    .await?;
                return Err(AdminLoginError::InvalidCredentials);
            }
            Err(e) => return Err(AdminLoginError::OtpUnavailable(e)),
        }

        // 5) 第五步 清零失败计数
        self.repository.clear_failed_login(&admin.id).await?;

        Ok(admin)
    }

    /// 幂等种子：保证 phone 对应一个**可登录的**超管（role=super_admin 且 status=active）。
    ///
    /// - 不存在 → 新建超管。密码是人挑的且过了策略校验，`must_change_password` 显式置 false
    ///   （表默认 TRUE 是给将来控制台 Provision 的临时密码用的）；
    /// - 已是超管 → `Unchanged`，一个字段都不动（超管恒 active，无需自愈；密码永不覆盖）；
    /// - 手机号被普通 admin 占用 → 拒绝（`PhoneTakenByNonSuperAdmin`）。seed 只**创建**超管，
    ///   不擅自把已存在账号提级（否则打错手机号即误提级）——冲突交运维手动处理。
    ///
    /// 幂等：对同一超管重复执行永不报错、永不重复建号；并发抢建时输家的唯一索引冲突
    /// 会回查并收敛成 `Unchanged`，不硬失败——可放心接进部署脚本。
    pub async fn seed_super_admin(
        &self,
        phone: &str,
        password: &str,
        display_name: &str,
    ) -> Result<SeedOutcome, AdminSeedError> {
        let phone = Phone::parse(phone).map_err(AdminSeedError::Phone)?;
        let psd = Password::parse(password).map_err(AdminSeedError::Password)?;
        match self
            .repository
            .get_by_phone(&phone.clone().into_string())
            .await
        {
            Ok(admin) => classify_existing(admin),
            Err(AdminRepositoryError::NotFound) => {
                let password_hash = psd.hash().await?;

                let new_admin = NewAdmin {
                    id: Uuid::now_v7(),
                    display_name: display_name.to_string(),
                    phone: phone.clone().into_string(),
                    password_hash,
                    role: AdminRole::SuperAdmin,
                    must_change_password: false,
                };
                match self.repository.create(new_admin).await {
                    Ok(admin) => Ok(SeedOutcome::Created(admin)),
                    // 并发竞态：另一个 seed/console 抢先建了同手机号。回查分类，
                    // 转成幂等结果（赢家建的是超管→Unchanged；是普通 admin→拒绝）。
                    Err(AdminRepositoryError::AlreadyExists) => classify_existing(
                        self.repository
                            .get_by_phone(&phone.clone().into_string())
                            .await?,
                    ),
                    Err(e) => Err(AdminSeedError::Repository(e)),
                }
            }
            Err(e) => Err(AdminSeedError::Repository(e)),
        }
    }
}

/// 已存在账号的归类：已是超管 → `Unchanged`（不动任何字段）；是普通 admin → 拒绝。
/// 不看普通 admin 的 status——无论 active/disabled，seed 都不擅自动它。
fn classify_existing(admin: Admin) -> Result<SeedOutcome, AdminSeedError> {
    if admin.role == AdminRole::SuperAdmin {
        Ok(SeedOutcome::Unchanged(admin))
    } else {
        Err(AdminSeedError::PhoneTakenByNonSuperAdmin)
    }
}
