use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    api::ApiJson,
    auth::{extract::AuthUser, handler::clean_refresh_token_cookie},
    error::{AppError, ErrorCode},
    otp::{model::Purpose, service::OtpServiceError},
    platform::{Password, PasswordError, hash_token},
    state::AppState,
    user::{
        model::{User, UserStatus},
        repository::{UserError, UserRepository},
        service::{normalize_identifier, verify_login_password},
    },
};

#[derive(Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContactChannel {
    Phone,
    Email,
}

#[derive(Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContactOperation {
    Bind,
    Unbind,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ContactVerificationCodeRequest {
    operation: ContactOperation,
    contact: String,
    verification_channel: ContactChannel,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ContactBindCodeRequest {
    contact: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ContactBindRequest {
    contact: String,
    code: String,
    verification_channel: ContactChannel,
    verification_code: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ContactUnbindRequest {
    channel: ContactChannel,
    verification_channel: ContactChannel,
    verification_code: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasswordForgotRequest {
    identifier: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasswordResetRequest {
    identifier: String,
    code: String,
    new_password: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasswordChangeRequest {
    current_password: String,
    new_password: String,
}

#[derive(Serialize, ToSchema)]
pub struct PasswordStatus {
    status: &'static str,
}

#[utoipa::path(post, path = "/api/v1/me/contact/verification-code", tag = "auth",
    security(("bearer_auth" = [])), request_body = ContactVerificationCodeRequest,
    responses((status = 204, description = "验证码已发送至本人已绑定渠道"),
        (status = 400, description = "目标或渠道无效"), (status = 401, description = "会话失效"),
        (status = 403, description = "不可解绑最后一种联系方式"),
        (status = 429, description = "发码频率超限"), (status = 503, description = "验证码服务不可用")))]
pub async fn verification_code(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(input): ApiJson<ContactVerificationCodeRequest>,
) -> Result<StatusCode, AppError> {
    let user = current_user(&state, &auth).await?;
    let contact = normalize(&input.contact)?;
    validate_operation(&user, input.operation, &contact)?;
    let target = channel_target(&user, input.verification_channel)?;
    let scope = contact_scope(&user, input.operation, &contact, target);
    state
        .otp_service
        .request_scoped(target, &scope, Purpose::ContactBind)
        .await
        .map_err(otp_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/me/contact/bind-code", tag = "auth",
    security(("bearer_auth" = [])), request_body = ContactBindCodeRequest,
    responses((status = 204, description = "新联系方式验证码已发送"),
        (status = 400, description = "目标无效或未变化"), (status = 401, description = "会话失效"),
        (status = 429, description = "发码频率超限"), (status = 503, description = "验证码服务不可用")))]
pub async fn bind_code(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(input): ApiJson<ContactBindCodeRequest>,
) -> Result<StatusCode, AppError> {
    let user = current_user(&state, &auth).await?;
    let contact = normalize(&input.contact)?;
    validate_operation(&user, ContactOperation::Bind, &contact)?;
    let scope = contact_scope(&user, ContactOperation::Bind, &contact, "new");
    state
        .otp_service
        .request_scoped(&contact, &scope, Purpose::ContactBind)
        .await
        .map_err(otp_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/me/contact/bind", tag = "auth",
    security(("bearer_auth" = [])), request_body = ContactBindRequest,
    responses((status = 204, description = "绑定或换绑成功，全部会话失效"),
        (status = 400, description = "目标无效或未变化"), (status = 401, description = "会话或验证码失效"),
        (status = 409, description = "联系方式已被其他账号占用"), (status = 503, description = "验证码服务不可用")))]
pub async fn bind(
    State(state): State<AppState>,
    auth: AuthUser,
    jar: CookieJar,
    ApiJson(input): ApiJson<ContactBindRequest>,
) -> Result<impl IntoResponse, AppError> {
    let contact = normalize(&input.contact)?;
    validate_code(&input.code)?;
    validate_code(&input.verification_code)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let user = lock_user(&mut tx, auth.subject).await?;
    check_session(&user, &auth)?;
    validate_operation(&user, ContactOperation::Bind, &contact)?;
    let target = channel_target(&user, input.verification_channel)?;
    let old_scope = contact_scope(&user, ContactOperation::Bind, &contact, target);
    let new_scope = contact_scope(&user, ContactOperation::Bind, &contact, "new");
    state
        .otp_service
        .verify(&old_scope, Purpose::ContactBind, &input.verification_code)
        .await
        .map_err(otp_error)?;
    state
        .otp_service
        .verify(&new_scope, Purpose::ContactBind, &input.code)
        .await
        .map_err(otp_error)?;
    let (phone, email) = if contact.contains('@') {
        (user.phone.as_deref(), Some(contact.as_str()))
    } else {
        (Some(contact.as_str()), user.email.as_deref())
    };
    sqlx::query("UPDATE users SET phone = $2, email = $3, security_version = security_version + 1, updated_at = NOW() WHERE id = $1")
        .bind(user.id).bind(phone).bind(email).execute(&mut *tx).await.map_err(contact_db_error)?;
    revoke_sessions(&mut tx, user.id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok((StatusCode::NO_CONTENT, clear_cookie(jar, &state)))
}

#[utoipa::path(post, path = "/api/v1/me/contact/unbind", tag = "auth",
    security(("bearer_auth" = [])), request_body = ContactUnbindRequest,
    responses((status = 204, description = "解绑成功，全部会话失效"),
        (status = 400, description = "渠道未绑定"), (status = 401, description = "会话或验证码失效"),
        (status = 403, description = "不可解绑最后一种联系方式"), (status = 503, description = "验证码服务不可用")))]
pub async fn unbind(
    State(state): State<AppState>,
    auth: AuthUser,
    jar: CookieJar,
    ApiJson(input): ApiJson<ContactUnbindRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_code(&input.verification_code)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let user = lock_user(&mut tx, auth.subject).await?;
    check_session(&user, &auth)?;
    let contact = channel_target(&user, input.channel)?;
    validate_operation(&user, ContactOperation::Unbind, contact)?;
    let target = channel_target(&user, input.verification_channel)?;
    let scope = contact_scope(&user, ContactOperation::Unbind, contact, target);
    state
        .otp_service
        .verify(&scope, Purpose::ContactBind, &input.verification_code)
        .await
        .map_err(otp_error)?;
    let (phone, email) = match input.channel {
        ContactChannel::Phone => (None, user.email.as_deref()),
        ContactChannel::Email => (user.phone.as_deref(), None),
    };
    sqlx::query("UPDATE users SET phone = $2, email = $3, security_version = security_version + 1, updated_at = NOW() WHERE id = $1")
        .bind(user.id).bind(phone).bind(email).execute(&mut *tx).await.map_err(AppError::internal)?;
    revoke_sessions(&mut tx, user.id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok((StatusCode::NO_CONTENT, clear_cookie(jar, &state)))
}

#[utoipa::path(post, path = "/api/v1/auth/password/forgot", tag = "auth",
    request_body = PasswordForgotRequest,
    responses((status = 200, description = "请求已受理，不区分账号是否存在或启用", body = PasswordStatus),
        (status = 400, description = "标识格式无效"), (status = 429, description = "发码频率超限"),
        (status = 503, description = "验证码服务不可用")))]
pub async fn forgot_password(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<PasswordForgotRequest>,
) -> Result<Json<PasswordStatus>, AppError> {
    let target = normalize(&input.identifier)?;
    let scope = reset_code_target(&state, &target).await?;
    // 所有格式合法目标走相同发送与限流路径，不能用频控响应枚举账号。
    state
        .otp_service
        .request_scoped(&target, &scope, Purpose::PasswordReset)
        .await
        .map_err(otp_error)?;
    Ok(Json(PasswordStatus { status: "ok" }))
}

#[utoipa::path(post, path = "/api/v1/auth/password/reset", tag = "auth",
    request_body = PasswordResetRequest,
    responses((status = 200, description = "密码已重置，全部会话失效", body = PasswordStatus),
        (status = 400, description = "标识或密码格式无效"), (status = 401, description = "验证码无效或账号不可用"),
        (status = 503, description = "验证码或密码哈希服务不可用")))]
pub async fn reset_password(
    State(state): State<AppState>,
    jar: CookieJar,
    ApiJson(input): ApiJson<PasswordResetRequest>,
) -> Result<impl IntoResponse, AppError> {
    let target = normalize(&input.identifier)?;
    let password = new_password(&input.new_password)?;
    validate_code(&input.code)?;
    let found = optional_user(&state, &target).await?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let user = match found {
        Some(user) => Some(lock_user(&mut tx, user.id).await?),
        None => None,
    };
    let scope = reset_scope(user.as_ref(), &target);
    state
        .otp_service
        .verify(&scope, Purpose::PasswordReset, &input.code)
        .await
        .map_err(otp_error)?;
    let user = user
        .filter(|u| {
            u.status == UserStatus::Active
                && (u.phone.as_deref() == Some(&target) || u.email.as_deref() == Some(&target))
        })
        .ok_or_else(invalid_code)?;
    let hash = password.hash().await.map_err(hash_error)?;
    update_password(&mut tx, user.id, &hash).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok((
        clear_cookie(jar, &state),
        Json(PasswordStatus { status: "ok" }),
    ))
}

#[utoipa::path(post, path = "/api/v1/auth/password/change", tag = "auth",
    security(("bearer_auth" = [])), request_body = PasswordChangeRequest,
    responses((status = 204, description = "密码已修改，全部会话失效"),
        (status = 400, description = "新密码格式无效或与原密码相同"), (status = 401, description = "会话失效或原密码错误"),
        (status = 503, description = "密码哈希服务不可用")))]
pub async fn change_password(
    State(state): State<AppState>,
    auth: AuthUser,
    jar: CookieJar,
    ApiJson(input): ApiJson<PasswordChangeRequest>,
) -> Result<impl IntoResponse, AppError> {
    let password = new_password(&input.new_password)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let user = lock_user(&mut tx, auth.subject).await?;
    check_session(&user, &auth)?;
    if !verify_login_password(&input.current_password, &user.password_hash).await {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidCredentials,
            "invalid credentials",
        ));
    }
    if verify_login_password(&input.new_password, &user.password_hash).await {
        return Err(AppError::validation(
            ErrorCode::PasswordUnchanged,
            "new_password",
            "password unchanged",
        ));
    }
    let hash = password.hash().await.map_err(hash_error)?;
    update_password(&mut tx, user.id, &hash).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok((StatusCode::NO_CONTENT, clear_cookie(jar, &state)))
}

pub(crate) fn new_password(value: &str) -> Result<Password, AppError> {
    if value.len() < 11 {
        return Err(super::handler::map_password_error(PasswordError::TooShort));
    }
    if value.len() > 20 {
        return Err(super::handler::map_password_error(PasswordError::TooLong));
    }
    if !value.bytes().all(|c| c.is_ascii_alphanumeric())
        || !value.bytes().any(|c| c.is_ascii_alphabetic())
        || !value.bytes().any(|c| c.is_ascii_digit())
    {
        return Err(AppError::validation(
            ErrorCode::InvalidPassword,
            "password",
            "password must contain letters and digits only",
        ));
    }
    Password::parse(&value.to_ascii_uppercase()).map_err(super::handler::map_password_error)
}

async fn current_user(state: &AppState, auth: &AuthUser) -> Result<User, AppError> {
    let user = UserRepository::new(state.pool.clone())
        .get_by_id(&auth.subject)
        .await
        .map_err(|e| match e {
            UserError::NotFound => invalid_session(),
            e => AppError::internal(e),
        })?;
    check_session(&user, auth)?;
    Ok(user)
}

async fn lock_user(connection: &mut PgConnection, id: Uuid) -> Result<User, AppError> {
    sqlx::query_as::<_, User>("SELECT id, phone, email, password_hash, security_version, display_name, last_active_role, created_at, updated_at, status, avatar_url FROM users WHERE id = $1 FOR UPDATE")
        .bind(id).fetch_optional(connection).await.map_err(AppError::internal)?.ok_or_else(invalid_session)
}

fn check_session(user: &User, auth: &AuthUser) -> Result<(), AppError> {
    if user.status != UserStatus::Active || user.security_version != auth.security_version {
        return Err(invalid_session());
    }
    Ok(())
}

fn channel_target(user: &User, channel: ContactChannel) -> Result<&str, AppError> {
    match channel {
        ContactChannel::Phone => user.phone.as_deref(),
        ContactChannel::Email => user.email.as_deref(),
    }
    .ok_or_else(|| {
        AppError::validation(
            ErrorCode::InvalidIdentifier,
            "channel",
            "contact channel unavailable",
        )
    })
}

fn validate_operation(
    user: &User,
    operation: ContactOperation,
    contact: &str,
) -> Result<(), AppError> {
    let matches = user.phone.as_deref() == Some(contact) || user.email.as_deref() == Some(contact);
    match operation {
        ContactOperation::Bind if matches => Err(AppError::validation(
            ErrorCode::InvalidIdentifier,
            "contact",
            "contact unchanged",
        )),
        ContactOperation::Unbind if !matches => Err(AppError::validation(
            ErrorCode::InvalidIdentifier,
            "contact",
            "contact channel unavailable",
        )),
        ContactOperation::Unbind if user.phone.is_none() || user.email.is_none() => Err(
            AppError::forbidden(ErrorCode::Forbidden, "cannot unbind last contact"),
        ),
        _ => Ok(()),
    }
}

fn contact_scope(
    user: &User,
    operation: ContactOperation,
    contact: &str,
    verifier: &str,
) -> String {
    let operation = match operation {
        ContactOperation::Bind => "bind",
        ContactOperation::Unbind => "unbind",
    };
    hash_token(
        &serde_json::json!([
            "contact",
            user.id,
            user.security_version,
            user.phone,
            user.email,
            operation,
            contact,
            verifier
        ])
        .to_string(),
    )
}

pub async fn login_code_target(state: &AppState, target: &str) -> Result<String, AppError> {
    let user = optional_user(state, target).await?;
    Ok(login_scope(user.as_ref(), target))
}

pub(crate) fn login_scope(user: Option<&User>, target: &str) -> String {
    hash_token(
        &serde_json::json!([
            "login",
            user.map(|u| u.id),
            user.map(|u| u.security_version),
            target
        ])
        .to_string(),
    )
}

pub(crate) async fn reset_code_target(state: &AppState, target: &str) -> Result<String, AppError> {
    let user = optional_user(state, target).await?;
    Ok(reset_scope(user.as_ref(), target))
}

fn reset_scope(user: Option<&User>, target: &str) -> String {
    hash_token(
        &serde_json::json!([
            "reset",
            user.map(|u| u.id),
            user.map(|u| u.security_version),
            user.map(|u| &u.password_hash),
            target
        ])
        .to_string(),
    )
}

pub(crate) async fn optional_user(
    state: &AppState,
    target: &str,
) -> Result<Option<User>, AppError> {
    match UserRepository::new(state.pool.clone())
        .get_by_identifier(target)
        .await
    {
        Ok(user) => Ok(Some(user)),
        Err(UserError::NotFound) => Ok(None),
        Err(e) => Err(AppError::internal(e)),
    }
}

async fn update_password(
    connection: &mut PgConnection,
    id: Uuid,
    hash: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE users SET password_hash = $2, security_version = security_version + 1, updated_at = NOW() WHERE id = $1")
        .bind(id).bind(hash).execute(&mut *connection).await.map_err(AppError::internal)?;
    revoke_sessions(connection, id).await
}

async fn revoke_sessions(connection: &mut PgConnection, id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = NOW() WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(connection)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

fn normalize(value: &str) -> Result<String, AppError> {
    normalize_identifier(value).map_err(|_| {
        AppError::validation(
            ErrorCode::InvalidIdentifier,
            "contact",
            "invalid identifier",
        )
    })
}

fn validate_code(code: &str) -> Result<(), AppError> {
    if code.len() != 6 || !code.bytes().all(|c| c.is_ascii_digit()) {
        return Err(invalid_code());
    }
    Ok(())
}

fn invalid_code() -> AppError {
    AppError::unauthorized(
        ErrorCode::InvalidOtpCode,
        "invalid or expired verification code",
    )
}

fn invalid_session() -> AppError {
    AppError::unauthorized(ErrorCode::InvalidToken, "invalid token")
}

fn otp_error(error: OtpServiceError) -> AppError {
    match error {
        OtpServiceError::InvalidCode => invalid_code(),
        OtpServiceError::RateLimited => {
            AppError::rate_limited(ErrorCode::OtpRateLimited, "too many requests")
        }
        e => AppError::unavailable_with_source(ErrorCode::OtpUnavailable, "OTP unavailable", e),
    }
}

fn hash_error(error: PasswordError) -> AppError {
    AppError::unavailable_with_source(
        ErrorCode::PasswordHashUnavailable,
        "password hash unavailable",
        error,
    )
}

fn clear_cookie(jar: CookieJar, state: &AppState) -> CookieJar {
    let mut cookie = clean_refresh_token_cookie();
    cookie.make_removal();
    cookie.set_http_only(true);
    cookie.set_same_site(axum_extra::extract::cookie::SameSite::Lax);
    cookie.set_secure(state.cookie_secure);
    // /me/contact 不会收到 Path=/auth 的 cookie，仍需显式下发过期指令。
    jar.add(cookie)
}

fn contact_db_error(error: sqlx::Error) -> AppError {
    if error
        .as_database_error()
        .is_some_and(|e| e.is_unique_violation())
    {
        AppError::conflict(
            ErrorCode::UserAlreadyExists,
            Some("contact"),
            "contact already registered",
        )
    } else {
        AppError::internal(error)
    }
}
