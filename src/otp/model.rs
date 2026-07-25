#[derive(sqlx::Type, PartialEq, Debug)]
#[sqlx(type_name = "channel", rename_all = "lowercase")]
pub enum Channel {
    Sms,
    Email,
}

impl Channel {
    pub fn for_target(target: &str) -> Self {
        if target.contains('@') {
            Channel::Email
        } else {
            Channel::Sms
        }
    }
}

#[derive(sqlx::Type, PartialEq, Debug, Clone, Copy, utoipa::ToSchema)]
#[sqlx(type_name = "purpose", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[schema(example = "login")]
pub enum Purpose {
    Login,           // 登录
    AdminLogin,      // 管理员登录
    AdminCreate,     // 管理员创建
    PasswordReset,   // 密码重置
    AccountDeletion, // 账户注销
    ContactBind,     // 联系方式绑定
}

impl Purpose {
    /// Redis key 里的 purpose 片段（snake_case）。**OtpStore 构造 key 必须用它**，
    /// 集成测试也用它——单一来源，编译期杜绝「测试与实现各写一份 snake_case 映射而漂移」。
    /// 值与 DB CHECK / `#[sqlx(rename_all="snake_case")]` 对齐（迁 Redis 后 DB 侧会随删表移除）。
    pub fn as_key_segment(&self) -> &'static str {
        match self {
            Purpose::Login => "login",
            Purpose::AdminLogin => "admin_login",
            Purpose::AdminCreate => "admin_create",
            Purpose::PasswordReset => "password_reset",
            Purpose::AccountDeletion => "account_deletion",
            Purpose::ContactBind => "contact_bind",
        }
    }
}
