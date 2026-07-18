use crate::{
    auth::{AUTH_MOUNT, REFRESH_TOKEN_COOKIE, extract::AuthUser},
    constant::TOKEN_SCHEMA,
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
    #[schema(example = "0198f2a1-3b4c-7d5e-8f90-1a2b3c4d5e6f")]
    id: Uuid,
    #[schema(example = "student@example.com")]
    email: Option<String>,
    #[schema(example = "13800138000")]
    phone: Option<String>,
    #[schema(example = "同学1234")]
    display_name: String,
    roles: Vec<UserRole>,
    token: Token,
    last_active_role: UserRole,
    #[schema(example = "https://cdn.example.com/avatar/default.png")]
    avatar_url: Option<String>,
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
    // user.id 是 Copy，先留底再把 user 整体交给 build_login_response，省一次 clone
    let user_id = user.id;
    let resp = build_login_response(&state, user).await?;
    let refresh_token_plaintext = refresh_token_plaintext(&state, user_id).await?;
    let cookie = refresh_cookie(
        refresh_token_plaintext,
        state.refresh_ttl.num_seconds(),
        state.cookie_secure,
    );
    let jar = jar.add(cookie);

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
    let id = normalize_identifier(&req.identifier);
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

    // 3) 查角色 + 发 token + 拼响应（与 login 共用）
    let user_id = user.id;
    let resp = build_login_response(&state, user).await?;

    // 生成refresh_token明文
    let refresh_token_plaintext = refresh_token_plaintext(&state, user_id).await?;

    // 生成cookie
    let cookie = refresh_cookie(
        refresh_token_plaintext,
        state.refresh_ttl.num_seconds(),
        state.cookie_secure,
    );
    let jar = jar.add(cookie);

    Ok((jar, Json(resp)))
}

/// 组装登录响应：查角色 + 签 access token + 拼 `LoginResponse`，**无 DB 副作用**。
/// `login` / `login_otp` 各自完成鉴权（密码 / OTP）后共用它，避免响应形状两处漂移。
/// refresh token 的签发（落库副作用）在各 handler 里、本函数成功之后——失败时不留孤儿 refresh。
async fn build_login_response(state: &AppState, user: User) -> Result<LoginResponse, AppError> {
    let roles = UserRepository::new(state.pool.clone())
        .get_roles_by_user_id(&user.id)
        .await
        .map_err(map_user_error)?;
    let token = generate_token(state, &user)
        .await
        .map_err(map_session_error)?;

    Ok(LoginResponse {
        id: user.id,
        email: user.email,
        phone: user.phone,
        display_name: user.display_name,
        roles,
        last_active_role: user.last_active_role.unwrap_or(UserRole::Student),
        avatar_url: Some(user.avatar_url),
        token,
    })
}

#[derive(Serialize, ToSchema)]
pub struct Token {
    #[schema(example = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIwMTk4Zi4uLiJ9.sig")]
    access_token: String,
    #[schema(example = "Bearer")]
    token_type: &'static str,
    /// access token 有效期（秒）
    #[schema(example = 900)]
    expires_in: i64,
}

async fn generate_token(state: &AppState, user: &User) -> Result<Token, SessionError> {
    // 1) 获取用户角色
    let role = user.last_active_role.unwrap_or(UserRole::Student).as_str();
    // 2) 生成 access token
    let access_token = state
        .token_manager
        .generate(user.id, role)
        .map_err(SessionError::Signing)?;

    Ok(Token {
        access_token,
        token_type: TOKEN_SCHEMA,
        expires_in: state.token_manager.ttl_seconds(),
    })
}

async fn refresh_token_plaintext(state: &AppState, user_id: Uuid) -> Result<String, AppError> {
    let session_svc = SessionService::new(
        RefreshTokenRepository::new(state.pool.clone()),
        state.refresh_ttl,
    );
    let refresh_token = session_svc
        .issue(user_id)
        .await
        .map_err(map_session_error)?;
    Ok(refresh_token.plaintext)
}

fn refresh_cookie(token: String, max_age_secs: i64, secure: bool) -> Cookie<'static> {
    Cookie::build((REFRESH_TOKEN_COOKIE, token))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path(AUTH_MOUNT)
        .secure(secure)
        .max_age(Duration::seconds(max_age_secs))
        .build()
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
        (status = 200, description = "轮换成功，返回新 access token；新 refresh token 经 Set-Cookie 下发", body = Token,
            headers(("Set-Cookie" = String, description = "轮换出的新 refresh_token cookie"))),
        (status = 401, description = "refresh token 缺失、无效、已过期或用户被禁用"),
    )
)]
pub async fn refresh_token(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, AppError> {
    // 1） 拿旧refresh token
    let refresh_token = jar
        .get(REFRESH_TOKEN_COOKIE)
        .map(|c| c.value().to_owned())
        .ok_or(AppError::Unauthenticated("invalid refresh token".into()))?;

    // 2） 用旧refresh token 换新refresh token
    let session_svc = SessionService::new(
        RefreshTokenRepository::new(state.pool.clone()),
        state.refresh_ttl,
    );
    let rotated_refresh = session_svc
        .rotate(refresh_token.as_str())
        .await
        .map_err(map_session_error)?;

    let user_repo = UserRepository::new(state.pool.clone());

    let user = user_repo
        .get_by_id(&rotated_refresh.user_id)
        .await
        .map_err(|e| match e {
            UserError::NotFound => AppError::Unauthenticated("invalid refresh token".into()),
            _ => AppError::internal(e),
        })?;

    if user.status != UserStatus::Active {
        return Err(AppError::Unauthenticated("invalid refresh token".into()));
    }

    let token = generate_token(&state, &user)
        .await
        .map_err(map_session_error)?;

    let cookie = refresh_cookie(
        rotated_refresh.refresh.plaintext,
        state.refresh_ttl.num_seconds(),
        state.cookie_secure,
    );
    let jar = jar.add(cookie);

    Ok((jar, Json(token)))
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
    if let Some(refresh_token) = jar.get(REFRESH_TOKEN_COOKIE).map(|c| c.value().to_owned()) {
        let session_svc = SessionService::new(
            RefreshTokenRepository::new(state.pool.clone()),
            state.refresh_ttl,
        );
        session_svc
            .logout(refresh_token.as_str())
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
pub struct Profile {
    #[schema(example = "0198f2a1-3b4c-7d5e-8f90-1a2b3c4d5e6f")]
    pub id: Uuid,
    #[schema(example = "同学1234")]
    pub name: String,
    #[schema(example = "student@example.com")]
    pub email: Option<String>,
    #[schema(example = "13800138000")]
    pub phone: Option<String>,
    pub roles: Vec<UserRole>,

    pub last_active_role: UserRole,
}

/// GET /api/v1/auth/me
#[utoipa::path(
    get,
    path = "/api/v1/auth/me",
    tag = "auth",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "当前登录用户信息", body = Profile),
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

    let roles = UserRepository::new(state.pool.clone())
        .get_roles_by_user_id(&user.id)
        .await
        .map_err(map_user_error)?;
    let last_active_role = user.last_active_role.unwrap_or(UserRole::Student);

    Ok((
        StatusCode::OK,
        Json(Profile {
            id: user.id,
            name: user.display_name,
            email: user.email,
            phone: user.phone,
            roles,
            last_active_role,
        }),
    ))
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
        // 仓储错 → 500，隐藏 cause
        LoginError::Repository(e) => AppError::internal(e),
    }
}
