pub mod auth;
mod extract;
mod model;
pub mod profile;
mod repository;
mod router;
mod service;
mod session;

pub use model::{Admin, AdminRole, AdminStatus, NewAdmin, SeedOutcome};
pub use repository::{AdminRepository, AdminRepositoryError};
pub use router::router;
pub use service::{AdminSeedError, AdminService};
pub use session::{
    AdminRefreshToken, AdminRefreshTokenError, AdminRefreshTokenRepository, AdminSessionError,
    AdminSessionService, IssuedAdminRefresh, NewAdminRefreshToken, RotatedAdminRefresh,
};

pub const ADMIN_MOUNT: &str = "/api/v1/admin";
pub const ADMIN_AUTH_MOUNT: &str = "/api/v1/admin/auth";
/// admin 的 refresh cookie 与 C 端（`refresh_token`）**必须**不同名不同 path——
/// 同名会互相覆盖，path 不隔离则浏览器会把 admin 的凭证带给 C 端接口。
pub const ADMIN_REFRESH_TOKEN_COOKIE: &str = "admin_refresh_token";
