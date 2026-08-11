use crate::{
    admin::{Admin, AdminAuth, AdminRepository, AdminRepositoryError},
    error::{AppError, ErrorCode},
    state::AppState,
};

/// 每次都按 token subject 回库核对最新状态，不信任可能过期的 role claim。
pub(crate) async fn require_active_admin(
    state: &AppState,
    auth: &AdminAuth,
) -> Result<Admin, AppError> {
    let admin = AdminRepository::new(state.pool.clone())
        .get_by_id(&auth.subject)
        .await
        .map_err(map_admin_error)?;

    if !admin.is_active() {
        return Err(AppError::forbidden(ErrorCode::AccountDisabled, "forbidden"));
    }
    if admin.must_change_password {
        return Err(AppError::forbidden(
            ErrorCode::MustChangePassword,
            "password change required",
        ));
    }
    Ok(admin)
}

pub(crate) async fn require_super_admin(
    state: &AppState,
    auth: &AdminAuth,
) -> Result<Admin, AppError> {
    let admin = require_active_admin(state, auth).await?;
    if !admin.is_super_admin() {
        return Err(AppError::forbidden(ErrorCode::Forbidden, "forbidden"));
    }
    Ok(admin)
}

fn map_admin_error(error: AdminRepositoryError) -> AppError {
    match error {
        AdminRepositoryError::NotFound => {
            AppError::unauthorized(ErrorCode::AdminNotFound, "admin not found")
        }
        other => AppError::internal(other),
    }
}
