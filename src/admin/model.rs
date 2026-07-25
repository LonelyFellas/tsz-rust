use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
}

#[derive(sqlx::Type, Debug, PartialEq, Clone, Copy)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AdminStatus {
    Active,
    Disabled,
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Admin {
    pub fn is_locked(&self, now: DateTime<Utc>) -> bool {
        self.locked_until
            .is_some_and(|locked_until| locked_until > now)
    }
    pub fn is_active(&self) -> bool {
        self.status == AdminStatus::Active
    }
    pub fn is_normal(&self, now: DateTime<Utc>) -> bool {
        self.status != AdminStatus::Disabled && !self.is_locked(now)
    }
}

pub struct NewAdmin {
    pub id: Uuid,
    pub display_name: String,
    pub phone: String,
    pub password_hash: String,
    pub role: AdminRole,
    pub must_change_password: bool,
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
