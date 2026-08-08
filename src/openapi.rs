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
    openapi::{
        Content, Ref, RefOr,
        path::Operation,
        security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
    },
};

/// 全局 API 文档。新增接口只需在 `paths` / `components.schemas` 两处登记。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "tsz-rust API",
        version = "0.1.0",
        description = "tsz 核心服务（Rust 版）接口文档"
    ),
    modifiers(&SecurityAddon, &ProblemDetailsAddon),
    paths(
        // auth 域
        crate::auth::handler::login,
        crate::auth::handler::login_otp,
        crate::auth::handler::refresh_token,
        crate::auth::handler::logout,
        crate::auth::handler::me,
        crate::auth::handler::register,
        // otp 域
        crate::otp::handler::send_otp,
        // admin 域
        crate::admin::auth::handler::admin_login,
        crate::admin::auth::handler::admin_refresh,
        crate::admin::auth::handler::admin_login_code,
        crate::admin::auth::handler::admin_logout,
        crate::admin::auth::handler::change_password,
        crate::admin::profile::handler::admin_profile,
        crate::admin::accounts::handler::create_admin,
        crate::admin::accounts::handler::request_create_admin_code,
        crate::admin::accounts::handler::list_admins,
        crate::admin::accounts::handler::list_users,
    ),
    components(
        schemas(
            crate::error::ErrorCode,
            crate::error::ProblemDetails,
            // auth
            crate::auth::handler::UserProfile,
            crate::auth::handler::LoginRequest,
            crate::auth::handler::LoginResponse,
            crate::auth::handler::LoginOtpRequest,
            crate::auth::handler::RegisterRequest,
            crate::auth::handler::Token,
            crate::auth::handler::RefreshResponse,
            // user
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
            crate::admin::AdminStatus,
            crate::admin::accounts::AdminAccountAdminResponse,
            crate::admin::accounts::AdminCreatorResponse,
            crate::admin::accounts::AdminAccountUserResponse,
            crate::admin::accounts::AdminUserListResponse,
            crate::api::PaginationMeta,
            crate::api::PaginatedResponse<crate::admin::accounts::AdminAccountAdminResponse>,
            crate::admin::accounts::handler::CreateAdminRequest,
            crate::admin::accounts::handler::CreateAdminResponse,
            crate::user::model::UserStatus,
        )
    ),
    tags(
        (name = "auth", description = "认证 / 会话"),
        (name = "user", description = "用户"),
        (name = "otp", description = "验证码"),
        (name = "admin", description = "管理后台认证 / 会话"),
        (name = "admin-accounts", description = "管理后台管理员账号治理"),
        (name = "admin-users", description = "管理后台 C 端用户管理"),
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

/// 给所有已声明的 4xx/5xx 自动挂上统一 ProblemDetails，避免每个 handler 重复标注。
struct ProblemDetailsAddon;

impl Modify for ProblemDetailsAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        for path in openapi.paths.paths.values_mut() {
            add_error_response_schema(path.get.as_mut());
            add_error_response_schema(path.put.as_mut());
            add_error_response_schema(path.post.as_mut());
            add_error_response_schema(path.delete.as_mut());
            add_error_response_schema(path.options.as_mut());
            add_error_response_schema(path.head.as_mut());
            add_error_response_schema(path.patch.as_mut());
            add_error_response_schema(path.trace.as_mut());
        }
    }
}

fn add_error_response_schema(operation: Option<&mut Operation>) {
    let Some(operation) = operation else {
        return;
    };
    for (status, response) in &mut operation.responses.responses {
        let is_error = status
            .parse::<u16>()
            .is_ok_and(|status| (400..600).contains(&status));
        if !is_error {
            continue;
        }
        if let RefOr::T(response) = response
            && response.content.is_empty()
        {
            response.content.insert(
                "application/problem+json".to_owned(),
                Content::new(Some(Ref::from_schema_name("ProblemDetails"))),
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
            ("post", "/api/v1/auth/register"),
            ("post", "/api/v1/otp/send"),
            ("post", "/api/v1/admin/auth/login"),
            ("post", "/api/v1/admin/auth/refresh"),
            ("post", "/api/v1/admin/auth/login-code"),
            ("post", "/api/v1/admin/auth/logout"),
            ("post", "/api/v1/admin/auth/change-password"),
            ("get", "/api/v1/admin/profile"),
            ("post", "/api/v1/admin/admins"),
            ("post", "/api/v1/admin/admins/create-code"),
            ("get", "/api/v1/admin/admins"),
            ("get", "/api/v1/admin/users"),
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
        let register = &json["paths"]["/api/v1/auth/register"]["post"];
        assert_eq!(
            register["summary"], "POST /api/v1/auth/register",
            "register 摘要不得残留旧 /user/register 路径"
        );
        assert!(
            register["description"]
                .as_str()
                .is_some_and(|text| text.contains("注册成功无需再次调用登录接口")),
            "register 描述应明确注册成功已经建立会话"
        );
        assert_eq!(
            register["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/RegisterRequest",
            "register 应引用 auth 域的请求 DTO"
        );
        assert_eq!(
            register["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/LoginResponse",
            "register 成功应直接返回登录响应"
        );
        let register_required = json["components"]["schemas"]["RegisterRequest"]["required"]
            .as_array()
            .expect("RegisterRequest 应声明必填字段");
        for field in ["phone", "password", "code"] {
            assert!(
                register_required.iter().any(|value| value == field),
                "RegisterRequest 应要求 {field}"
            );
        }
        assert!(
            json["components"]["schemas"]["RegisterRequest"]["properties"]
                .get("email")
                .is_none(),
            "当前注册契约不应暴露 email"
        );
        assert_eq!(
            json["components"]["schemas"]["RegisterRequest"]["properties"]["phone"]["description"],
            "中国大陆手机号",
            "注册 phone 描述应明确当前只支持手机号"
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

        let create_admin = &json["paths"]["/api/v1/admin/admins"]["post"];
        assert_eq!(
            create_admin["security"][0]["bearer_auth"],
            serde_json::json!([]),
            "创建管理员接口必须声明 Bearer 鉴权"
        );
        assert_eq!(
            create_admin["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/CreateAdminRequest",
            "创建管理员接口应引用明确的请求 DTO"
        );
        assert_eq!(
            create_admin["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/CreateAdminResponse",
            "创建管理员接口的 201 应引用明确的响应 DTO"
        );
        for status in ["400", "401", "403", "409", "500", "503"] {
            assert!(
                create_admin["responses"][status].is_object(),
                "创建管理员接口应声明 {status} 响应"
            );
        }
        let required = json["components"]["schemas"]["CreateAdminRequest"]["required"]
            .as_array()
            .expect("CreateAdminRequest 应声明必填字段");
        for field in ["phone", "code"] {
            assert!(
                required.iter().any(|value| value == field),
                "CreateAdminRequest 应要求 {field}"
            );
        }
        assert!(
            !required.iter().any(|value| value == "display_name"),
            "display_name 可由系统生成，不应标记为必填"
        );

        let create_admin_code = &json["paths"]["/api/v1/admin/admins/create-code"]["post"];
        assert_eq!(
            create_admin_code["security"][0]["bearer_auth"],
            serde_json::json!([]),
            "创建管理员发码接口必须声明 Bearer 鉴权"
        );
        for status in ["202", "401", "403", "429", "503"] {
            assert!(
                create_admin_code["responses"][status].is_object(),
                "创建管理员发码接口应声明 {status} 响应"
            );
        }

        let list_admins = &json["paths"]["/api/v1/admin/admins"]["get"];
        assert_eq!(
            list_admins["security"][0]["bearer_auth"],
            serde_json::json!([]),
            "管理员列表接口必须声明 Bearer 鉴权"
        );
        for status in ["200", "400", "401", "403", "500"] {
            assert!(
                list_admins["responses"][status].is_object(),
                "管理员列表接口应声明 {status} 响应"
            );
        }

        let parameters = list_admins["parameters"]
            .as_array()
            .expect("管理员列表接口应声明查询参数");
        for name in ["role", "phone", "display_name", "page", "page_size"] {
            assert!(
                parameters
                    .iter()
                    .any(|parameter| parameter["name"] == name && parameter["in"] == "query"),
                "管理员列表接口应声明 query 参数 {name}"
            );
        }
        let page = parameters
            .iter()
            .find(|parameter| parameter["name"] == "page")
            .expect("管理员列表接口应声明 page");
        assert_eq!(page["schema"]["default"], 1);
        assert_eq!(page["schema"]["minimum"], 1);
        let page_size = parameters
            .iter()
            .find(|parameter| parameter["name"] == "page_size")
            .expect("管理员列表接口应声明 page_size");
        assert_eq!(page_size["schema"]["default"], 20);
        assert_eq!(page_size["schema"]["minimum"], 1);
        assert_eq!(page_size["schema"]["maximum"], 100);

        let list_schema_ref =
            list_admins["responses"]["200"]["content"]["application/json"]["schema"]["$ref"]
                .as_str()
                .expect("管理员列表 200 响应应引用分页响应 schema");
        let list_schema_name = list_schema_ref
            .strip_prefix("#/components/schemas/")
            .expect("管理员列表响应应引用 components schema");
        let list_properties = &json["components"]["schemas"][list_schema_name]["properties"];
        assert!(list_properties["items"].is_object());
        assert!(list_properties["pagination"].is_object());

        let pagination_properties = &json["components"]["schemas"]["PaginationMeta"]["properties"];
        for field in ["page", "page_size", "total", "total_pages"] {
            assert!(
                pagination_properties[field].is_object(),
                "PaginationMeta schema 应包含 {field}"
            );
        }
        assert!(
            json["components"]["schemas"]["AdminAccountAdminResponse"]["properties"]["created_by"]
                .is_object(),
            "管理员公开响应 schema 应包含 created_by"
        );

        let list_users = &json["paths"]["/api/v1/admin/users"]["get"];
        assert_eq!(
            list_users["security"][0]["bearer_auth"],
            serde_json::json!([]),
            "用户列表接口必须声明 Bearer 鉴权"
        );
        for status in ["200", "400", "401", "403", "500"] {
            assert!(
                list_users["responses"][status].is_object(),
                "用户列表接口应声明 {status} 响应"
            );
        }
        let user_parameters = list_users["parameters"]
            .as_array()
            .expect("用户列表接口应声明查询参数");
        for name in [
            "role",
            "q",
            "registered_from",
            "registered_to",
            "page",
            "page_size",
        ] {
            assert!(
                user_parameters
                    .iter()
                    .any(|parameter| parameter["name"] == name && parameter["in"] == "query"),
                "用户列表接口应声明 query 参数 {name}"
            );
        }
        for parameter in user_parameters {
            assert_ne!(
                parameter["required"],
                serde_json::json!(true),
                "用户列表筛选参数都应可选：{parameter}"
            );
        }
        assert_eq!(
            list_users["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/AdminUserListResponse"
        );
        let user_list_properties =
            &json["components"]["schemas"]["AdminUserListResponse"]["properties"];
        assert!(user_list_properties["items"].is_object());
        assert!(user_list_properties["page"].is_object());
        let user_properties =
            &json["components"]["schemas"]["AdminAccountUserResponse"]["properties"];
        for field in [
            "id",
            "phone",
            "email",
            "display_name",
            "avatar_url",
            "roles",
            "status",
            "created_at",
            "updated_at",
        ] {
            assert!(
                user_properties[field].is_object(),
                "AdminAccountUserResponse schema 应包含 {field}"
            );
        }
        let user_required = json["components"]["schemas"]["AdminAccountUserResponse"]["required"]
            .as_array()
            .expect("AdminAccountUserResponse required 应为数组");
        for field in ["phone", "email"] {
            assert_eq!(
                user_properties[field]["type"], "string",
                "{field} 应为可选的非 null string"
            );
            assert!(
                !user_required.iter().any(|required| required == field),
                "{field} 不应出现在 required 中"
            );
        }
        assert!(
            json["paths"]["/api/v1/admin/admins/users"].is_null(),
            "用户列表不得继续暴露在错误的 /api/v1/admin/admins/users 路径"
        );
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

    #[test]
    fn error_contract_is_documented() {
        let json = serde_json::to_value(ApiDoc::openapi()).unwrap();
        assert!(json["components"]["schemas"]["ErrorCode"].is_object());
        assert!(json["components"]["schemas"]["ProblemDetails"].is_object());

        let properties = &json["components"]["schemas"]["ProblemDetails"]["properties"];
        for field in ["type", "title", "status", "detail", "code", "field"] {
            assert!(properties[field].is_object(), "ProblemDetails 缺少 {field}");
        }
        assert!(properties["error"].is_null());

        let schema = &json["paths"]["/api/v1/auth/register"]["post"]["responses"]["400"]["content"]
            ["application/problem+json"]["schema"];
        assert_eq!(schema["$ref"], "#/components/schemas/ProblemDetails");

        for (path_name, path) in json["paths"].as_object().unwrap() {
            for (method, operation) in path.as_object().unwrap() {
                if ![
                    "get", "put", "post", "delete", "options", "head", "patch", "trace",
                ]
                .contains(&method.as_str())
                {
                    continue;
                }
                for (status, response) in operation["responses"].as_object().unwrap() {
                    if status
                        .parse::<u16>()
                        .is_ok_and(|status| (400..600).contains(&status))
                    {
                        assert_eq!(
                            response["content"]["application/problem+json"]["schema"]["$ref"],
                            "#/components/schemas/ProblemDetails",
                            "{method} {path_name} 的 {status} 应使用 ProblemDetails"
                        );
                        assert!(
                            response["content"]["application/json"].is_null(),
                            "错误响应不得继续声明 application/json"
                        );
                    }
                }
            }
        }
    }
}
