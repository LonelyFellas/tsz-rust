use uuid::Uuid;

use crate::{
    admin::{
        Admin, AdminRepository, AdminRepositoryError, AdminRole, AdminStatus, NewAdmin,
        model::SeedOutcome,
    },
    platform::{
        Password, PasswordError, dummy_hash, hash_password, normalize_phone, verify_password,
    },
};

#[derive(Debug, thiserror::Error)]
pub enum AdminLoginError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("account is disabled")]
    AccountDisabled,
    #[error(transparent)]
    Repository(#[from] AdminRepositoryError),
}

#[derive(Debug, thiserror::Error)]
pub enum AdminSeedError {
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
}

impl AdminService {
    pub fn new(repository: AdminRepository) -> Self {
        Self { repository }
    }
    pub async fn authenticate(
        &self,
        phone: &str,
        password: &str,
    ) -> Result<Admin, AdminLoginError> {
        let phone = normalize_phone(phone);
        match self.repository.get_by_phone(&phone).await {
            Ok(admin) => {
                if !verify_password(password.to_string(), admin.password_hash.clone()).await {
                    return Err(AdminLoginError::InvalidCredentials);
                }
                if admin.status == AdminStatus::Disabled {
                    return Err(AdminLoginError::AccountDisabled);
                }
                Ok(admin)
            }
            Err(AdminRepositoryError::NotFound) => {
                verify_password(password.to_string(), dummy_hash().to_string()).await;
                Err(AdminLoginError::InvalidCredentials)
            }
            Err(e) => Err(AdminLoginError::Repository(e)),
        }
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
        let phone = normalize_phone(phone);
        match self.repository.get_by_phone(&phone).await {
            Ok(admin) => classify_existing(admin),
            Err(AdminRepositoryError::NotFound) => {
                let password = Password::parse(password).map_err(AdminSeedError::Password)?;
                let password_hash = hash_password(password.into_string()).await?;

                let new_admin = NewAdmin {
                    id: Uuid::now_v7(),
                    display_name: display_name.to_string(),
                    phone: phone.clone(),
                    password_hash,
                    role: AdminRole::SuperAdmin,
                    must_change_password: false,
                };
                match self.repository.create(new_admin).await {
                    Ok(admin) => Ok(SeedOutcome::Created(admin)),
                    // 并发竞态：另一个 seed/console 抢先建了同手机号。回查分类，
                    // 转成幂等结果（赢家建的是超管→Unchanged；是普通 admin→拒绝）。
                    Err(AdminRepositoryError::AlreadyExists) => {
                        classify_existing(self.repository.get_by_phone(&phone).await?)
                    }
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
