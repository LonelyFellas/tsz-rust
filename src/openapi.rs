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
        // admin 域
        crate::admin::auth::handler::admin_login,
        crate::admin::auth::handler::admin_refresh,
        crate::admin::auth::handler::admin_login_code,
        crate::admin::auth::handler::admin_logout,
        crate::admin::auth::handler::change_password,
        crate::admin::profile::handler::admin_profile,
    ),
    components(
        schemas(
            // auth
            crate::auth::handler::UserProfile,
            crate::auth::handler::LoginRequest,
            crate::auth::handler::LoginResponse,
            crate::auth::handler::LoginOtpRequest,
            crate::auth::handler::Token,
            crate::auth::handler::RefreshResponse,
            // user
            crate::user::handler::RegisterRequest,
            crate::user::handler::RegisterResponse,
            crate::user::model::UserRole,
            // otp
            crate::otp::handler::SendOtpRequest,
            crate::otp::model::Purpose,
            // admin
            crate::admin::auth::handler::AdminLoginRequest,
            crate::admin::auth::handler::AdminLoginResponse,
            crate::admin::auth::handler::AdminRefreshResponse,
            crate::admin::auth::handler::AdminLoginOtpRequest,
            crate::admin::auth::handler::ChangePasswordRequest,
            crate::admin::auth::handler::AdminProfile,
            crate::admin::profile::handler::AdminProfileResponse,
            crate::admin::auth::handler::AdminToken,
            crate::admin::AdminRole,
        )
    ),
    tags(
        (name = "auth", description = "认证 / 会话"),
        (name = "user", description = "用户"),
        (name = "otp", description = "验证码"),
        (name = "admin", description = "管理后台认证 / 会话"),
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
            ("post", "/api/v1/admin/auth/login"),
            ("post", "/api/v1/admin/auth/refresh"),
            ("post", "/api/v1/admin/auth/login-code"),
            ("post", "/api/v1/admin/auth/logout"),
            ("post", "/api/v1/admin/auth/change-password"),
            ("get", "/api/v1/admin/profile"),
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
            json["components"]["schemas"]["UserProfile"].is_object(),
            "UserProfile schema 应出现在 spec 中"
        );
        assert!(
            json["components"]["schemas"]["RefreshResponse"].is_object(),
            "RefreshResponse schema 应出现在 spec 中（refresh 响应含 refresh_token_expires_at，不是裸 Token）"
        );
        let change_password = &json["paths"]["/api/v1/admin/auth/change-password"]["post"];
        assert_eq!(
            change_password["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ChangePasswordRequest",
            "change-password 应引用明确的请求 DTO"
        );
        assert_eq!(
            change_password["security"][0]["bearer_auth"],
            serde_json::json!([]),
            "change-password 必须声明 Bearer 鉴权"
        );
        let change_password_required =
            json["components"]["schemas"]["ChangePasswordRequest"]["required"]
                .as_array()
                .expect("ChangePasswordRequest 应声明必填字段");
        for field in ["current_password", "new_password"] {
            assert!(
                change_password_required.iter().any(|value| value == field),
                "ChangePasswordRequest 应要求 {field}"
            );
        }
        // profile 响应：flatten+inline 的 4 字段概要 + permissions 必须都出现在 schema 里
        // （utoipa 对 serde flatten 字段需要 #[schema(inline)]，漏了 spec 会缺概要字段）。
        let profile_props = &json["components"]["schemas"]["AdminProfileResponse"]["properties"];
        for field in ["id", "phone", "display_name", "role", "permissions"] {
            assert!(
                profile_props[field].is_object(),
                "AdminProfileResponse schema 应含 {field} 字段（flatten 展开后），实际：{profile_props}"
            );
        }
    }

    /// cookie 契约必须写进 spec：refresh/logout 的入参是 Cookie 而非 body——
    /// 不声明的话，照 swagger 生成的客户端会以为这两个 POST 无需任何凭证，调用必 401。
    /// login/login-otp/refresh 的 200 同理要声明 Set-Cookie 响应头。
    #[test]
    fn cookie_contract_is_documented() {
        let spec = ApiDoc::openapi();
        let json = serde_json::to_value(&spec).expect("spec 应能序列化为 JSON");

        // refresh 与 logout 都声明了名为 refresh_token 的 Cookie 参数
        for path in ["/api/v1/auth/refresh", "/api/v1/auth/logout"] {
            let params = json["paths"][path]["post"]["parameters"]
                .as_array()
                .unwrap_or_else(|| panic!("{path} 应声明 parameters（refresh_token cookie）"));
            assert!(
                params
                    .iter()
                    .any(|p| p["name"] == "refresh_token" && p["in"] == "cookie"),
                "{path} 的 parameters 里应有 in=cookie 的 refresh_token，实际：{params:?}"
            );
        }

        // 下发 refresh cookie 的三个成功响应都声明了 Set-Cookie 头
        for (path, status) in [
            ("/api/v1/auth/login", "200"),
            ("/api/v1/auth/login-otp", "200"),
            ("/api/v1/auth/refresh", "200"),
            ("/api/v1/auth/logout", "204"),
        ] {
            assert!(
                json["paths"][path]["post"]["responses"][status]["headers"]["Set-Cookie"]
                    .is_object(),
                "{path} 的 {status} 响应应声明 Set-Cookie 头"
            );
        }

        // logout 已幂等化：spec 里不得再出现 401
        assert!(
            json["paths"]["/api/v1/auth/logout"]["post"]["responses"]["401"].is_null(),
            "logout 无失败分支，401 应从 spec 移除"
        );

        // —— admin 域同样的 cookie 契约（名字/路径与 C 端隔离，见 ADMIN_REFRESH_TOKEN_COOKIE）——
        // admin refresh 与 logout 都声明了 admin_refresh_token cookie 参数
        for path in ["/api/v1/admin/auth/refresh", "/api/v1/admin/auth/logout"] {
            let params = json["paths"][path]["post"]["parameters"]
                .as_array()
                .unwrap_or_else(|| {
                    panic!("{path} 应声明 parameters（admin_refresh_token cookie）")
                });
            assert!(
                params
                    .iter()
                    .any(|p| p["name"] == "admin_refresh_token" && p["in"] == "cookie"),
                "{path} 的 parameters 里应有 in=cookie 的 admin_refresh_token，实际：{params:?}"
            );
        }

        // admin login 200 / refresh 200 / logout 204 都声明了 Set-Cookie 头
        for (path, status) in [
            ("/api/v1/admin/auth/login", "200"),
            ("/api/v1/admin/auth/refresh", "200"),
            ("/api/v1/admin/auth/logout", "204"),
        ] {
            assert!(
                json["paths"][path]["post"]["responses"][status]["headers"]["Set-Cookie"]
                    .is_object(),
                "{path} 的 {status} 响应应声明 Set-Cookie 头"
            );
        }

        // admin login-code 的反枚举契约：只有 202 成功态，绝不能出现 401/403/429 等
        // 可探测态（那会把「这号是不是管理员」暴露成 oracle）。
        for leaky in ["401", "403", "423", "429"] {
            assert!(
                json["paths"]["/api/v1/admin/auth/login-code"]["post"]["responses"][leaky]
                    .is_null(),
                "admin login-code 反枚举契约：不得声明可探测状态码 {leaky}"
            );
        }
    }
}
