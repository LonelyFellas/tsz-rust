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
        AdminRepository, AdminRepositoryError, AdminRole, AdminService, AdminSessionError,
        AdminSessionService, service::AdminLoginError,
    },
    error::AppError,
    otp::{model::Purpose, service::OtpServiceError},
    platform::Phone,
    state::AppState,
};

fn admin_svc(state: &AppState) -> AdminService {
    AdminService::new(
        AdminRepository::new(state.pool.clone()),
        state.otp_service.clone(),
    )
}

#[derive(Serialize, ToSchema)]
pub struct AdminRefreshResponse {
    #[serde(flatten)]
    token: AdminToken,
    /// refresh token 过期时间（Unix 秒，绝对时间戳，与轮换出的新枚落库值一致）
    #[schema(example = 1752566400)]
    refresh_token_expires_at: i64,
}

/// POST /api/v1/admin/auth/refresh
///
/// 用 admin_refresh_token cookie 轮换会话：签发新 access token，并经 Set-Cookie
/// 下发轮换出的新 refresh token（继承旧枚绝对死线，Q8 不续命）。旧枚原子消费作废；
/// 窗口外重放会连坐吊销该 admin 全部会话。
#[utoipa::path(
    post,
    path = "/api/v1/admin/auth/refresh",
    tag = "admin",
    params(
        ("admin_refresh_token" = String, Cookie,
            description = "登录时经 Set-Cookie 下发的 admin refresh token，浏览器自动携带（HttpOnly；Path=/api/v1/admin/auth，仅认证端点可见）"),
    ),
    responses(
        (status = 200, description = "轮换成功，返回新 access token；新 refresh token 经 Set-Cookie 下发", body = AdminRefreshResponse,
            headers(("Set-Cookie" = String, description = "轮换出的新 admin_refresh_token cookie"))),
        (status = 401, description = "refresh token 缺失、无效、已过期或重放"),
        (status = 403, description = "账号被禁用"),
        (status = 423, description = "账号被临时锁定"),
    )
)]
pub async fn admin_refresh(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, AppError> {
    // 1） 拿refresh plaintext
    let refresh_plaintext = jar
        .get(ADMIN_REFRESH_TOKEN_COOKIE)
        .map(|c| c.value().to_owned())
        .ok_or(AppError::Unauthenticated("invalid refresh token".into()))?;
    // 2) 先查用户id
    let admin_id = admin_session_svc(&state)
        .peek_admin_id(&refresh_plaintext)
        .await
        .map_err(map_admin_session_error)?;

    // 3) 查用户
    let admin_id = admin_id.ok_or(AppError::Unauthenticated("invalid refresh token".into()))?;
    let admin = AdminRepository::new(state.pool.clone())
        .get_by_id(&admin_id)
        .await
        .map_err(|e| match e {
            AdminRepositoryError::NotFound => {
                AppError::Unauthenticated("invalid refresh token".into())
            }
            _ => AppError::internal(e),
        })?;

    // 4) 查看状态
    if !admin.is_active() {
        return Err(AppError::Forbidden);
    }
    // 5) 查看是否被锁住
    if admin.is_locked(Utc::now()) {
        return Err(AppError::Locked("admin is locked".into()));
    }
    // 6) 签发token
    let token = generate_admin_token(&state, &admin).map_err(map_admin_session_error)?;

    // 7） 用旧refresh token 换新refresh token
    let rotated_refresh = admin_session_svc(&state)
        .rotate(&refresh_plaintext)
        .await
        .map_err(map_admin_session_error)?;

    // 8) 下发新refresh token
    let jar = jar.add(admin_refresh_cookie(
        rotated_refresh.refresh.plaintext,
        &state,
    ));

    // 9) 返回响应
    Ok((
        jar,
        Json(AdminRefreshResponse {
            token,
            refresh_token_expires_at: rotated_refresh.refresh.expires_at.timestamp(),
        }),
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct AdminLoginRequest {
    /// 登录标识：手机号
    #[schema(example = "13800138000")]
    pub phone: String,
    /// 短信验证码（6 位；`login-code` 端点发出）
    #[schema(example = "123456")]
    pub code: String,
    /// 登录密码
    #[schema(example = "password123")]
    pub password: String,
}

#[derive(Serialize, ToSchema)]
pub struct AdminLoginResponse {
    admin_profile: AdminProfile,
    #[serde(flatten)]
    token: AdminToken,
    /// refresh token 过期时间（Unix 秒，绝对时间戳，与落库那枚一致）
    refresh_token_expires_at: i64,
}

/// POST /api/v1/admin/auth/login
///
/// admin 登录须**三要素齐全**：手机号 + 密码 + 短信验证码（先调 `login-code` 取码）。
/// 失败一律压成逐字节一致的 401（密码错 / 码错 / 查无此号不可区分，反枚举）；
/// 禁用 403、锁定 423、验证码基础设施抖动 503 是仅有的可区分分支。
#[utoipa::path(
    post,
    path = "/api/v1/admin/auth/login",
    tag = "admin",
    request_body = AdminLoginRequest,
    responses(
        (status = 200, description = "登录成功，返回管理员档案与令牌", body = AdminLoginResponse,
            headers(("Set-Cookie" = String,
                description = "admin_refresh_token cookie（HttpOnly; SameSite=Lax; Path=/api/v1/admin/auth; Max-Age=refresh TTL 秒）"))),
        (status = 400, description = "手机号格式非法"),
        (status = 401, description = "凭证无效（密码/验证码错误或账号不存在，逐字节不可区分）"),
        (status = 403, description = "账号被禁用"),
        (status = 423, description = "账号因多次登录失败被临时锁定"),
        (status = 429, description = "验证码请求过于频繁"),
        (status = 503, description = "验证码基础设施不可用（Redis/短信）"),
    )
)]
pub async fn admin_login(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<AdminLoginRequest>,
) -> Result<impl IntoResponse, AppError> {
    let admin = admin_svc(&state)
        .login(&req.phone, &req.password, &req.code)
        .await
        .map_err(map_admin_login_error)?;

    // 拼响应：签 access（可失败、无副作用）→ 发 refresh cookie（唯一 DB 副作用，压轴）
    let (jar, resp) = build_admin_login_response(&state, admin, jar).await?;

    // 返回响应
    Ok((jar, Json(resp)))
}

/// POST /api/v1/admin/auth/logout
/// 幂等登出（对齐 RFC 7009 语义）：cookie 缺失 = 已处于登出态，目标已达成。
/// 有枚就吊销；无论有没有，都下发清除 cookie 并返回 204。
#[utoipa::path(
    post,
    path = "/api/v1/admin/auth/logout",
    tag = "admin",
    params(
        ("admin_refresh_token" = Option<String>, Cookie,
            description = "要吊销的 admin refresh token；缺失时仍 204（幂等）"),
    ),
    responses(
        (status = 204, description = "登出成功（幂等，无失败分支）；带 cookie 时附清除 Set-Cookie",
            headers(("Set-Cookie" = String, description = "清除 admin_refresh_token 的 cookie（Max-Age=0）"))),
    )
)]
pub async fn admin_logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, AppError> {
    if let Some(cookie) = jar.get(ADMIN_REFRESH_TOKEN_COOKIE) {
        admin_session_svc(&state)
            .logout(cookie.value())
            .await
            .map_err(map_admin_session_error)?;
    }

    let jar = jar.remove(clean_admin_refresh_cookie());
    Ok((jar, StatusCode::NO_CONTENT))
}

#[derive(Deserialize, ToSchema)]
pub struct AdminLoginOtpRequest {
    /// 登录标识：手机号
    #[schema(example = "13800138000")]
    pub phone: String,
}

/// POST /api/v1/admin/auth/login-code
///
/// 返回类型刻意钉成具体的 `StatusCode`（而非 `impl IntoResponse`）：本端点的反枚举契约
/// 是「一切非基础设施结果恒返 202 空 body」，具体类型让「某分支误返 Json(..) 破坏该契约」
/// 变成编译错误。
#[utoipa::path(
    post,
    path = "/api/v1/admin/auth/login-code",
    tag = "admin",
    request_body = AdminLoginOtpRequest,
    responses(
        (status = 202, description = "已受理（反枚举：查无此号/禁用/锁定/冷却中一律 202 空 body，仅 active admin 真正发码）"),
        (status = 400, description = "手机号格式非法"),
        (status = 503, description = "验证码基础设施不可用（Redis/短信）"),
    )
)]
pub async fn admin_login_code(
    State(state): State<AppState>,
    Json(req): Json<AdminLoginOtpRequest>,
) -> Result<StatusCode, AppError> {
    let phone = Phone::parse(&req.phone).map_err(|e| AppError::BadRequest(e.to_string()))?;

    // 只有 active 且未锁的 admin 才真正发码；其余一律静默压平成 202（反名录枚举）。
    // 用 or-pattern 把「行为相同」的分支并到一起，让它们必须一起改——避免有人日后给
    // 其中一条静默分支加日志/指标而漏掉孪生分支，重新打开时序/行为侧信道。
    match AdminRepository::new(state.pool.clone())
        .get_by_phone(phone.as_str())
        .await
    {
        Ok(admin) if admin.is_normal(Utc::now()) => {
            match state
                .otp_service
                .request(phone.as_str(), Purpose::AdminLogin)
                .await
            {
                Ok(()) | Err(OtpServiceError::RateLimited) => {} // 已发 / 冷却·日限：静默
                Err(_) => return Err(AppError::ServiceUnavailable), // Redis/sender 故障：503
            }
        }
        Ok(_) | Err(AdminRepositoryError::NotFound) => {} // 禁用·锁定 / 查无此号：静默
        Err(e) => return Err(AppError::internal(e)),      // DB 真故障：500
    };

    Ok(StatusCode::ACCEPTED)
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
fn admin_session_svc(state: &AppState) -> AdminSessionService {
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
    let refresh = admin_session_svc(state)
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
        AdminLoginError::Phone(e) => AppError::BadRequest(e.to_string()),
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
