use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(sqlx::Type, Debug, PartialEq, Clone, Copy, Serialize, Deserialize, utoipa::ToSchema)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AdminRole {
    SuperAdmin,
    Admin,
}

impl AdminRole {
    pub fn as_str(&self) -> &str {
        match self {
            AdminRole::SuperAdmin => "super_admin",
            AdminRole::Admin => "admin",
        }
    }
    pub fn parse(role: &str) -> Option<AdminRole> {
        match role {
            "super_admin" => Some(AdminRole::SuperAdmin),
            "admin" => Some(AdminRole::Admin),
            _ => None,
        }
    }
}

#[derive(sqlx::Type, Debug, PartialEq, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AdminStatus {
    Active,
    Disabled,
}

/// 管理员的英语方言偏好（英美方言偏好化 A1）：账号级个人设置，默认英式。
/// 它只决定 admin 端的录入与展示口径，**不是词条属性**——同一条词条的英美并列拼写
/// 由 `lexicon.entry_headwords` 承载，不因某个管理员偏好英式就消失。
// example 必须挂在枚举定义上：字段级 `#[schema(example = ...)]` 对枚举字段会被 utoipa 静默丢弃
// （枚举生成裸 $ref，挂不住兄弟属性）。
#[derive(sqlx::Type, Debug, PartialEq, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[schema(example = "uk")]
pub enum AdminDialectPreference {
    /// 英式（默认）。
    Uk,
    /// 美式。
    Us,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Admin {
    pub id: Uuid,
    pub display_name: String,
    pub phone: String,
    pub password_hash: String,
    pub role: AdminRole,
    pub status: AdminStatus,
    pub must_change_password: bool,
    pub failed_login_count: i32,
    pub locked_until: Option<DateTime<Utc>>,
    pub created_by_admin_id: Option<Uuid>,
    pub dialect_preference: AdminDialectPreference,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Admin {
    // 用户是否被锁定🔒
    pub fn is_locked(&self, now: DateTime<Utc>) -> bool {
        self.locked_until
            .is_some_and(|locked_until| locked_until > now)
    }
    // 用户是否是正常活跃状态
    pub fn is_active(&self) -> bool {
        self.status == AdminStatus::Active
    }
    // 用户是否是正常状态。即没有被锁定，也没有活跃状态。
    pub fn is_normal(&self, now: DateTime<Utc>) -> bool {
        self.status != AdminStatus::Disabled && !self.is_locked(now)
    }

    pub fn is_super_admin(&self) -> bool {
        self.role == AdminRole::SuperAdmin
    }
}

pub struct NewAdmin {
    pub id: Uuid,
    pub display_name: String,
    pub phone: String,
    pub password_hash: String,
    pub role: AdminRole,
    pub must_change_password: bool,
    pub created_by_admin_id: Option<Uuid>,
}

#[derive(Debug, PartialEq)]
pub enum SeedOutcome {
    Created(Admin),   // 新建超管
    Unchanged(Admin), // 手机号已是超管，什么都没改（超管恒 active，无需自愈）
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    /// 只关心 locked_until，其余字段填占位——is_locked 不看它们。
    fn admin_with_lock(locked_until: Option<DateTime<Utc>>) -> Admin {
        Admin {
            id: Uuid::now_v7(),
            display_name: "占位".into(),
            phone: "13800138000".into(),
            password_hash: "x".into(),
            role: AdminRole::Admin,
            status: AdminStatus::Active,
            must_change_password: false,
            failed_login_count: 0,
            locked_until,
            created_by_admin_id: None,
            dialect_preference: AdminDialectPreference::Uk,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // is_locked 是「谁能登录 / 谁能收码」的单一判据（login 第①步与发码门禁共用）——
    // 边界钉死，否则 login-code 和 login 各自重写会漂移（正是 #4 的病根）。

    #[test]
    fn future_lock_is_locked() {
        let now = Utc::now();
        let admin = admin_with_lock(Some(now + Duration::minutes(10)));
        assert!(admin.is_locked(now), "locked_until 在未来应视为锁定");
    }

    #[test]
    fn past_lock_is_not_locked() {
        // 过去的 locked_until 不是锁（自动解锁，无 cron）——设计 §3。
        let now = Utc::now();
        let admin = admin_with_lock(Some(now - Duration::minutes(1)));
        assert!(!admin.is_locked(now), "已过期的 locked_until 不应视为锁定");
    }

    #[test]
    fn none_lock_is_not_locked() {
        let admin = admin_with_lock(None);
        assert!(
            !admin.is_locked(Utc::now()),
            "locked_until 为 None 不应视为锁定"
        );
    }

    #[test]
    fn exactly_now_is_not_locked() {
        // 判据是严格 `> now`：截止时刻整点即视为已解锁（边界不含），
        // 与 register_failed_login 里 `locked_until > NOW()` 的 SQL 判据一致。
        let now = Utc::now();
        let admin = admin_with_lock(Some(now));
        assert!(
            !admin.is_locked(now),
            "locked_until == now 应视为已解锁（严格大于）"
        );
    }
}
