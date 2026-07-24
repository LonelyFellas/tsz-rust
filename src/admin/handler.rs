use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use axum_extra::extract::{
    CookieJar,
    cookie::{Cookie, SameSite},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use time::Duration as TimeDuration;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    admin::{
        ADMIN_AUTH_MOUNT, ADMIN_REFRESH_TOKEN_COOKIE, Admin, AdminRefreshTokenRepository,
        AdminRepository, AdminRole, AdminService, AdminSessionError, AdminSessionService,
        service::AdminLoginError,
    },
    error::AppError,
    state::AppState,
};

#[derive(Deserialize)]
pub struct AdminLoginRequest {
    pub phone: String,
    pub code: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct AdminLoginResponse {
    admin_profile: AdminProfile,
    #[serde(flatten)]
    token: AdminToken,
    /// refresh token 过期时间（Unix 秒，绝对时间戳，与落库那枚一致）
    refresh_token_expires_at: i64,
}

/// POST /admin/login
pub async fn admin_login(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<AdminLoginRequest>,
) -> Result<impl IntoResponse, AppError> {
    let admin_svc = AdminService::new(
        AdminRepository::new(state.pool.clone()),
        state.otp_service.clone(),
    );
    let admin = admin_svc
        .login(&req.phone, &req.password, &req.code)
        .await
        .map_err(map_admin_login_error)?;

    // 拼响应：签 access（可失败、无副作用）→ 发 refresh cookie（唯一 DB 副作用，压轴）
    let (jar, resp) = build_admin_login_response(&state, admin, jar).await?;

    // 返回响应
    Ok((jar, Json(resp)))
}

/// POST /admin/logout
/// 幂等登出（对齐 RFC 7009 语义）：cookie 缺失 = 已处于登出态，目标已达成。
/// 有枚就吊销；无论有没有，都下发清除 cookie 并返回 204。
pub async fn admin_logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, AppError> {
    if let Some(cookie) = jar.get(ADMIN_REFRESH_TOKEN_COOKIE) {
        admin_session_service(&state)
            .logout(cookie.value())
            .await
            .map_err(map_admin_session_error)?;
    }

    let jar = jar.remove(clean_admin_refresh_cookie());
    Ok((jar, StatusCode::NO_CONTENT))
}

#[derive(Serialize, ToSchema)]
pub struct AdminProfile {
    pub id: Uuid,
    pub display_name: String,
    pub phone: String,
    pub role: AdminRole,
}

#[derive(Serialize, ToSchema)]
pub struct AdminToken {
    access_token: String,
    /// access token 有效期（秒）
    expires_in: i64,
}

/// 组装登录响应：拼档案 + 签 access token（可失败、无副作用）→ 签发 refresh cookie
/// （唯一 DB 副作用，压轴）。落库之前任何一步失败都零副作用、不留孤儿 refresh——
/// 与 C 端 `build_login_response` 同一模式。
async fn build_admin_login_response(
    state: &AppState,
    admin: Admin,
    jar: CookieJar,
) -> Result<(CookieJar, AdminLoginResponse), AppError> {
    let admin_profile = build_admin_profile(&admin);
    let token = generate_admin_token(state, &admin).map_err(map_admin_session_error)?;
    let (jar, refresh_token_expires_at) = issue_admin_refresh_cookie(state, jar, admin.id).await?;

    Ok((
        jar,
        AdminLoginResponse {
            admin_profile,
            token,
            refresh_token_expires_at: refresh_token_expires_at.timestamp(),
        },
    ))
}

/// admin 的 role 就在 admins 行上（单列，不像 user 有独立角色表）——直接取，不回库。
fn build_admin_profile(admin: &Admin) -> AdminProfile {
    AdminProfile {
        id: admin.id,
        display_name: admin.display_name.clone(),
        phone: admin.phone.clone(),
        role: admin.role,
    }
}

fn generate_admin_token(state: &AppState, admin: &Admin) -> Result<AdminToken, AdminSessionError> {
    let access_token = state
        .admin_token_manager
        .generate(admin.id, admin.role.as_str())
        .map_err(AdminSessionError::Signing)?;

    Ok(AdminToken {
        access_token,
        expires_in: state.admin_token_manager.ttl_seconds(),
    })
}

/// 域内统一的 `AdminSessionService` 装配点——repo/ttl 接线只写一次。
fn admin_session_service(state: &AppState) -> AdminSessionService {
    AdminSessionService::new(
        AdminRefreshTokenRepository::new(state.pool.clone()),
        state.admin_refresh_ttl,
    )
}

/// 签发一枚新 admin refresh（落库，Q1 会先清场旧会话）并挂上 cookie。
/// refresh 轮换**不**走这里——新枚由 `rotate` 原子产出，handler 只管装 cookie。
async fn issue_admin_refresh_cookie(
    state: &AppState,
    jar: CookieJar,
    admin_id: Uuid,
) -> Result<(CookieJar, DateTime<Utc>), AppError> {
    let refresh = admin_session_service(state)
        .issue(&admin_id)
        .await
        .map_err(map_admin_session_error)?;
    Ok((
        jar.add(admin_refresh_cookie(refresh.plaintext, state)),
        refresh.expires_at,
    ))
}

/// admin refresh cookie 的唯一构造点：安全属性（HttpOnly/SameSite/Path/Secure/Max-Age）
/// 全在这。Path 钉在 `ADMIN_AUTH_MOUNT`（/api/v1/admin/auth）——只随认证端点
/// （login/logout/refresh）发送，不随 ADMIN_MOUNT 下的业务请求外泄；名字也与 C 端隔离
/// （见 `ADMIN_REFRESH_TOKEN_COOKIE` 的注释）。TTL 与 Secure 直接读 state，调用方无从传错。
fn admin_refresh_cookie(token: String, state: &AppState) -> Cookie<'static> {
    Cookie::build((ADMIN_REFRESH_TOKEN_COOKIE, token))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path(ADMIN_AUTH_MOUNT)
        .secure(state.cookie_secure)
        .max_age(TimeDuration::seconds(state.admin_refresh_ttl.num_seconds()))
        .build()
}

/// `jar.remove` 按「名字 + Path」生成删除 cookie（Max-Age=0 由它自己设），
/// 只需这两项与下发时一致；Path 不匹配则浏览器视为另一枚 cookie，清不掉——
/// 故与 `admin_refresh_cookie` 一样钉 `ADMIN_AUTH_MOUNT`。
fn clean_admin_refresh_cookie() -> Cookie<'static> {
    Cookie::build(ADMIN_REFRESH_TOKEN_COOKIE)
        .path(ADMIN_AUTH_MOUNT)
        .build()
}

fn map_admin_login_error(err: AdminLoginError) -> AppError {
    match err {
        AdminLoginError::InvalidCredentials => {
            AppError::Unauthenticated("invalid credentials".into())
        }
        AdminLoginError::AccountDisabled => AppError::Forbidden,
        AdminLoginError::Locked => AppError::Locked(
            "account temporarily locked due to too many failed login attempts".into(),
        ),
        AdminLoginError::OtpUnavailable(e) => match e {
            crate::otp::service::OtpServiceError::RateLimited => AppError::TooManyRequests,
            crate::otp::service::OtpServiceError::Store(_)
            | crate::otp::service::OtpServiceError::Send(_) => AppError::ServiceUnavailable,
            crate::otp::service::OtpServiceError::InvalidCode => {
                AppError::internal(anyhow::anyhow!("InvalidCode 应已映射为 InvalidCredentials"))
            }
        },
        AdminLoginError::Repository(e) => AppError::internal(e),
    }
}

fn map_admin_session_error(err: AdminSessionError) -> AppError {
    match err {
        AdminSessionError::InvalidRefreshToken => {
            AppError::Unauthenticated("invalid refresh token".into())
        }
        AdminSessionError::Repository(e) => AppError::internal(e),
        AdminSessionError::Signing(e) => AppError::internal(e),
    }
}
