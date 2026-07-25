use crate::{
    auth::{AUTH_MOUNT, REFRESH_TOKEN_COOKIE, extract::AuthUser},
    error::AppError,
    otp::{model::Purpose, service::OtpServiceError},
    session::{
        repository::RefreshTokenRepository,
        service::{SessionError, SessionService},
    },
    state::AppState,
    user::{
        model::{User, UserRole, UserStatus},
        repository::{UserError, UserRepository},
        service::{LoginError, UserService, normalize_identifier},
    },
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use axum_extra::extract::{
    CookieJar,
    cookie::{Cookie, SameSite},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use time::Duration;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    /// 登录标识：手机号或邮箱
    #[schema(example = "student@example.com")]
    pub identifier: String,
    #[schema(example = "P@ssw0rd!")]
    pub password: String,
}

#[derive(Serialize, ToSchema)]
pub struct LoginResponse {
    user: UserProfile,
    #[serde(flatten)]
    token: Token,
    /// refresh token 过期时间（Unix 秒，绝对时间戳，与落库那枚一致）
    #[schema(example = 1752566400)]
    refresh_token_expires_at: i64,
}

/// POST /api/v1/auth/login
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "auth",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "登录成功，返回用户信息与令牌", body = LoginResponse,
            headers(("Set-Cookie" = String,
                description = "refresh_token cookie（HttpOnly; SameSite=Lax; Path=/api/v1/auth; Max-Age=refresh TTL 秒）"))),
        (status = 401, description = "凭证无效（用户不存在或密码错误，不可区分）"),
        (status = 403, description = "账号被禁用"),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<LoginRequest>,
) -> Result<impl IntoResponse, AppError> {
    // 1) 验凭证：业务全在 user 域，handler 只调。失败→统一错误（不可区分）
    let user_sve = UserService::new(UserRepository::new(state.pool.clone()));
    let user = user_sve
        .authenticate(&req.identifier, &req.password)
        .await
        .map_err(map_login_error)?;

    // 2) 查角色 + 发 token + 拼响应（与 login_otp 共用 build_login_response）

    let (jar, resp) = build_login_response(&state, user, jar).await?;

    Ok((jar, Json(resp)))
}

#[derive(Deserialize, ToSchema)]
pub struct LoginOtpRequest {
    /// 登录标识：手机号或邮箱
    #[schema(example = "13800138000")]
    pub identifier: String,
    /// 收到的 6 位验证码
    #[schema(example = "123456")]
    pub code: String,
}

/// POST /api/v1/auth/login-otp
#[utoipa::path(
    post,
    path = "/api/v1/auth/login-otp",
    tag = "auth",
    request_body = LoginOtpRequest,
    responses(
        (status = 200, description = "验证码登录成功，返回用户信息与令牌", body = LoginResponse,
            headers(("Set-Cookie" = String,
                description = "refresh_token cookie（HttpOnly; SameSite=Lax; Path=/api/v1/auth; Max-Age=refresh TTL 秒）"))),
        (status = 401, description = "验证码无效或已过期"),
        (status = 429, description = "请求过于频繁"),
    )
)]
pub async fn login_otp(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<LoginOtpRequest>,
) -> Result<impl IntoResponse, AppError> {
    // 1) 先验码——purpose 写死 Login，不接受客户端传入：
    //    否则持一枚 password_reset 码的人可显式指定 purpose，拿重置码换登录会话，
    //    绕过 purpose 隔离。请求体里多带的 purpose 字段会被 serde 直接忽略。
    let id =
        normalize_identifier(&req.identifier).map_err(|e| AppError::BadRequest(e.to_string()))?;
    state
        .otp_service
        .verify(&id, Purpose::Login, &req.code)
        .await
        .map_err(map_login_otp_error)?;

    // 2) 查活跃用户
    let user_sve = UserService::new(UserRepository::new(state.pool.clone()));
    let user = user_sve
        .find_active_by_identifier(&id)
        .await
        .map_err(map_login_error)?;

    // 3) 查角色 + 发 token + 拼响应 + 发 refresh cookie（与 login 共用）
    let (jar, resp) = build_login_response(&state, user, jar).await?;
    Ok((jar, Json(resp)))
}

/// 组装登录响应：查角色 + 签 access token（可失败、无副作用）→ 签发 refresh cookie
/// （唯一 DB 副作用，压轴）→ 拼响应。落库之前任何一步失败都零副作用、不留孤儿 refresh，
/// 与 `refresh_token` 的「rotate 压轴」同一模式。
/// `login` / `login_otp` 各自完成鉴权（密码 / OTP）后共用它，避免响应形状两处漂移。
async fn build_login_response(
    state: &AppState,
    user: User,
    jar: CookieJar,
) -> Result<(CookieJar, LoginResponse), AppError> {
    let profile = load_user_profile(state, &user).await?;
    let token = generate_token(state, &user)
        .await
        .map_err(map_session_error)?;
    let (jar, refresh_token_expires_at) = issue_refresh_cookie(state, jar, user.id).await?;

    Ok((
        jar,
        LoginResponse {
            user: profile,
            token,
            refresh_token_expires_at: refresh_token_expires_at.timestamp(),
        },
    ))
}

/// login / me 共用的 user 档案装配点：查角色 + 拼 `UserProfile`（可失败、无 DB 副作用）。
/// **唯一**构造点——将来 user 对象加字段（如 status/created_at，见契约 0.1 订正）只改这里，
/// login 响应里的 user 与 me 的响应形状才不会漂移。
async fn load_user_profile(state: &AppState, user: &User) -> Result<UserProfile, AppError> {
    let roles = UserRepository::new(state.pool.clone())
        .get_roles_by_user_id(&user.id)
        .await
        .map_err(map_user_error)?;

    Ok(UserProfile {
        id: user.id,
        display_name: user.display_name.clone(),
        email: user.email.clone(),
        phone: user.phone.clone(),
        avatar_url: user.avatar_url.clone(),
        roles,
        active_role: user.active_role(),
    })
}

#[derive(Serialize, ToSchema)]
pub struct Token {
    #[schema(example = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIwMTk4Zi4uLiJ9.sig")]
    access_token: String,
    /// access token 有效期（秒）
    #[schema(example = 900)]
    expires_in: i64,
}

async fn generate_token(state: &AppState, user: &User) -> Result<Token, SessionError> {
    // 1) 获取用户角色
    let role = user.active_role().as_str();
    // 2) 生成 access token
    let access_token = state
        .token_manager
        .generate(user.id, role)
        .map_err(SessionError::Signing)?;

    Ok(Token {
        access_token,
        expires_in: state.token_manager.ttl_seconds(),
    })
}

/// 域内统一的 `SessionService` 装配点——repo/ttl 接线只写一次，
/// 将来 service 加依赖（如 redis）只改这里。
fn session_service(state: &AppState) -> SessionService {
    SessionService::new(
        RefreshTokenRepository::new(state.pool.clone()),
        state.refresh_ttl,
    )
}

/// 签发一枚新 refresh（落库）并挂上 cookie。`login` / `login_otp`（将来 register
/// 自动登录）共用；refresh 轮换**不**走这里——新枚由 `rotate` 原子产出，handler 只管装 cookie。
async fn issue_refresh_cookie(
    state: &AppState,
    jar: CookieJar,
    user_id: Uuid,
) -> Result<(CookieJar, DateTime<Utc>), AppError> {
    let refresh = session_service(state)
        .issue(user_id)
        .await
        .map_err(map_session_error)?;
    Ok((
        jar.add(refresh_cookie(refresh.plaintext, state)),
        refresh.expires_at,
    ))
}

/// refresh cookie 的唯一构造点：安全属性（HttpOnly/SameSite/Path/Secure/Max-Age）全在这，
/// TTL 与 Secure 直接读 state，调用方无从传错。
fn refresh_cookie(token: String, state: &AppState) -> Cookie<'static> {
    Cookie::build((REFRESH_TOKEN_COOKIE, token))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path(AUTH_MOUNT)
        .secure(state.cookie_secure)
        .max_age(Duration::seconds(state.refresh_ttl.num_seconds()))
        .build()
}

#[derive(Serialize, ToSchema)]
pub struct RefreshResponse {
    #[serde(flatten)]
    token: Token,
    /// refresh token 过期时间（Unix 秒，绝对时间戳，与轮换出的新枚落库值一致）
    #[schema(example = 1752566400)]
    refresh_token_expires_at: i64,
}

/// POST /api/v1/auth/refresh
#[utoipa::path(
    post,
    path = "/api/v1/auth/refresh",
    tag = "auth",
    params(
        ("refresh_token" = String, Cookie,
            description = "登录时经 Set-Cookie 下发的 refresh token，浏览器自动携带（HttpOnly，手动调用需自带 Cookie 头）"),
    ),
    responses(
        (status = 200, description = "轮换成功，返回新 access token；新 refresh token 经 Set-Cookie 下发", body = RefreshResponse,
            headers(("Set-Cookie" = String, description = "轮换出的新 refresh_token cookie"))),
        (status = 401, description = "refresh token 缺失、无效、已过期或用户被禁用"),
    )
)]
pub async fn refresh_token(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, AppError> {
    // 1） 拿refresh plaintext
    let refresh_plaintext = jar
        .get(REFRESH_TOKEN_COOKIE)
        .map(|c| c.value().to_owned())
        .ok_or(AppError::Unauthenticated("invalid refresh token".into()))?;
    // 2) 先查用户id
    let user_id = session_service(&state)
        .peek_user_id(&refresh_plaintext)
        .await
        .map_err(map_session_error)?;

    // 3) 查用户
    let user_id = user_id.ok_or(AppError::Unauthenticated("invalid refresh token".into()))?;
    let user = UserRepository::new(state.pool.clone())
        .get_by_id(&user_id)
        .await
        .map_err(|e| match e {
            UserError::NotFound => AppError::Unauthenticated("invalid refresh token".into()),
            _ => AppError::internal(e),
        })?;
    // 4) 查用户状态
    if user.status != UserStatus::Active {
        return Err(AppError::Unauthenticated("invalid refresh token".into()));
    }

    // 5) 签发token
    let token = generate_token(&state, &user)
        .await
        .map_err(map_session_error)?;

    // 6） 用旧refresh token 换新refresh token
    let rotated_refresh = session_service(&state)
        .rotate(&refresh_plaintext)
        .await
        .map_err(map_session_error)?;

    // 7) 下发新refresh token
    let jar = jar.add(refresh_cookie(rotated_refresh.refresh.plaintext, &state));

    // 8) 返回响应
    Ok((
        jar,
        Json(RefreshResponse {
            token,
            refresh_token_expires_at: rotated_refresh.refresh.expires_at.timestamp(),
        }),
    ))
}

/// POST /api/v1/auth/logout
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    tag = "auth",
    params(
        ("refresh_token" = Option<String>, Cookie,
            description = "要吊销的 refresh token；缺失时仍 204（幂等）"),
    ),
    responses(
        (status = 204, description = "登出成功（幂等，无失败分支）；带 cookie 时附清除 Set-Cookie",
            headers(("Set-Cookie" = String, description = "清除 refresh_token 的 cookie（Max-Age=0）"))),
    )
)]
pub async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, AppError> {
    // 幂等登出(对齐 RFC 7009 语义):cookie 缺失 = 已处于登出态,目标已达成。
    // 有枚就吊销;无论有没有,都下发清除 cookie 并返回 204。
    if let Some(cookie) = jar.get(REFRESH_TOKEN_COOKIE) {
        session_service(&state)
            .logout(cookie.value())
            .await
            .map_err(map_session_error)?;
    }

    let jar = jar.remove(clean_refresh_token_cookie());
    Ok((jar, StatusCode::NO_CONTENT))
}

/// `jar.remove` 按「名字 + Path」生成删除 cookie（Max-Age=0 由它自己设），
/// 只需这两项与下发时一致；Path 不匹配则浏览器视为另一枚 cookie，清不掉。
fn clean_refresh_token_cookie() -> Cookie<'static> {
    Cookie::build(REFRESH_TOKEN_COOKIE).path(AUTH_MOUNT).build()
}

#[derive(Serialize, ToSchema)]
pub struct UserProfile {
    #[schema(example = "0198f2a1-3b4c-7d5e-8f90-1a2b3c4d5e6f")]
    pub id: Uuid,
    #[schema(example = "同学1234")]
    pub display_name: String,
    #[schema(example = "student@example.com")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[schema(example = "13800138000")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// 头像未实现：现恒为空串 ""（不是 null、不省略，契约 0.1），实现后才是 URL
    #[schema(example = "")]
    pub avatar_url: String,
    pub roles: Vec<UserRole>,
    pub active_role: UserRole,
}

/// GET /api/v1/auth/me
#[utoipa::path(
    get,
    path = "/api/v1/auth/me",
    tag = "auth",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "当前登录用户信息", body = UserProfile),
        (status = 401, description = "未认证 / token 无效或过期"),
    )
)]
pub async fn me(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let user = UserRepository::new(state.pool.clone())
        .get_by_id(&user.subject)
        .await
        .map_err(map_user_error)?;

    if user.status != UserStatus::Active {
        return Err(AppError::Unauthenticated("user is not active".into()));
    }

    let profile = load_user_profile(&state, &user).await?;

    Ok((StatusCode::OK, Json(profile)))
}

fn map_login_otp_error(err: OtpServiceError) -> AppError {
    match err {
        OtpServiceError::InvalidCode => AppError::Unauthenticated("invalid code".into()),
        OtpServiceError::RateLimited => AppError::TooManyRequests,
        _ => AppError::internal(err),
    }
}

fn map_user_error(err: UserError) -> AppError {
    match err {
        UserError::NotFound => AppError::Unauthenticated("user not found".into()),
        _ => AppError::internal(err),
    }
}

fn map_session_error(err: SessionError) -> AppError {
    match err {
        SessionError::InvalidRefreshToken => {
            AppError::Unauthenticated("invalid refresh token".into())
        }
        SessionError::Repository(e) => AppError::internal(e),
        SessionError::Signing(e) => AppError::internal(e),
    }
}

fn map_login_error(err: LoginError) -> AppError {
    match err {
        // 用户不存在 / 密码错 —— 统一 401，不可区分（安全铁律）
        LoginError::InvalidCredentials => AppError::Unauthenticated("invalid credentials".into()),
        // 账号被禁：密码已验证后才可能到这，可如实告知
        LoginError::AccountDisabled => AppError::Forbidden,
        LoginError::IdentifierInvalid => AppError::BadRequest("identifier is invalid".into()),
        // 仓储错 → 500，隐藏 cause
        LoginError::Repository(e) => AppError::internal(e),
    }
}
