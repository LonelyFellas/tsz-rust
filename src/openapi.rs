//! OpenAPI 文档聚合入口。
//!
//! 规范（往上加接口时照此办理）：
//! 1. handler 上加 `#[utoipa::path(...)]`，写清 method / path / tag / responses；
//!    带鉴权的接口再加 `security(("bearer_auth" = []))`。
//! 2. 请求/响应 DTO（`XxxRequest` / `XxxResponse`）加 `#[derive(ToSchema)]`。
//! 3. 把 handler 登记到下面 `paths(...)`，把 DTO 登记到 `components(schemas(...))`。
//! 4. path 里的 `path = "..."` 要带上 nest 前缀（如 `/api/v1/auth/me`），
//!    因为 utoipa 不感知 axum 的 `.nest()`，前缀得手写全。

use utoipa::{
    Modify, OpenApi,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};

/// 全局 API 文档。新增接口只需在 `paths` / `components.schemas` 两处登记。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "tsz-rust API",
        version = "0.1.0",
        description = "tsz 核心服务（Rust 版）接口文档"
    ),
    modifiers(&SecurityAddon),
    paths(
        // auth 域
        crate::auth::handler::login,
        crate::auth::handler::login_otp,
        crate::auth::handler::refresh_token,
        crate::auth::handler::logout,
        crate::auth::handler::me,
        // user 域
        crate::user::handler::register,
        // otp 域
        crate::otp::handler::send_otp,
    ),
    components(
        schemas(
            // auth
            crate::auth::handler::Profile,
            crate::auth::handler::LoginRequest,
            crate::auth::handler::LoginResponse,
            crate::auth::handler::LoginOtpRequest,
            crate::auth::handler::RefreshTokenRequest,
            crate::auth::handler::LogoutRequest,
            crate::auth::handler::Token,
            // user
            crate::user::handler::RegisterRequest,
            crate::user::handler::RegisterResponse,
            crate::user::model::UserRole,
            // otp
            crate::otp::handler::SendOtpRequest,
            crate::otp::model::Purpose,
        )
    ),
    tags(
        (name = "auth", description = "认证 / 会话"),
        (name = "user", description = "用户"),
        (name = "otp", description = "验证码"),
    )
)]
pub struct ApiDoc;

/// 注入 Bearer JWT 安全方案，让 Swagger UI 出现 "Authorize" 按钮。
/// 接口上用 `security(("bearer_auth" = []))` 引用这个名字。
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        // components 一定存在（derive 已建好），直接取用
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .build(),
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// spec 能生成、能序列化，且示范接口 + 安全方案都在。
    /// 往上加接口时若忘了在 `paths(...)` 登记，这里的断言会替你兜住。
    #[test]
    fn openapi_spec_is_well_formed() {
        let spec = ApiDoc::openapi();
        let json = serde_json::to_value(&spec).expect("spec 应能序列化为 JSON");

        // 所有已标注路由都在（注意带 nest 前缀）。漏登记会在这里失败。
        for (method, path) in [
            ("post", "/api/v1/auth/login"),
            ("post", "/api/v1/auth/login-otp"),
            ("post", "/api/v1/auth/refresh"),
            ("post", "/api/v1/auth/logout"),
            ("get", "/api/v1/auth/me"),
            ("post", "/api/v1/user/register"),
            ("post", "/api/v1/otp/send"),
        ] {
            assert!(
                json["paths"][path][method].is_object(),
                "{} {} 应出现在 spec 中",
                method.to_uppercase(),
                path
            );
        }
        // Bearer 安全方案已注入
        assert_eq!(
            json["components"]["securitySchemes"]["bearer_auth"]["scheme"],
            "bearer"
        );
        // 响应 DTO 已登记
        assert!(
            json["components"]["schemas"]["Profile"].is_object(),
            "Profile schema 应出现在 spec 中"
        );
    }
}
