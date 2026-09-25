use axum::{
    extract::{MatchedPath, Request, State},
    http::Method,
    middleware::Next,
    response::Response,
};

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

pub(crate) async fn enforce_business_access(
    State(state): State<AppState>,
    auth: AdminAuth,
    path: MatchedPath,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    if !business_access_allowed(
        admin.is_super_admin(),
        false,
        request.method(),
        path.as_str(),
    ) {
        let can_publish = crate::admin::publication_permission::effective(&state.pool, admin.id)
            .await
            .map_err(AppError::internal)?;
        if !business_access_allowed(false, can_publish, request.method(), path.as_str()) {
            return Err(AppError::forbidden(ErrorCode::Forbidden, "forbidden"));
        }
    }
    Ok(next.run(request).await)
}

fn business_access_allowed(
    is_super_admin: bool,
    can_publish_lexicon: bool,
    method: &Method,
    path: &str,
) -> bool {
    if is_super_admin || matches!(*method, Method::GET | Method::HEAD) {
        return true;
    }
    if method != Method::POST {
        return false;
    }
    if path.ends_with("/lexicon/entries/component-targets/search") {
        return true;
    }
    can_publish_lexicon
        && [
            "/lexicon/entries/{id}/publications",
            "/lexicon/entries/{id}/validate",
            "/lexicon/entries/{id}/steps/forms/impact",
            "/lexicon/entries/{id}/archive",
            "/lexicon/entries/{id}/restore",
            "/lexicon/entries/archive-batch",
            "/lexicon/entries/restore-batch",
            "/lexicon/entries/publications/batch",
            "/lexicon/entries/{id}/publications/{publication_id}/rollback",
            "/lexicon/sentences/{id}/publications",
            "/lexicon/sentences/{id}/publications/{publication_id}/rollback",
            "/lexicon/sentences/{id}/withdraw",
            "/lexicon/sentences/{id}/restore",
        ]
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

fn map_admin_error(error: AdminRepositoryError) -> AppError {
    match error {
        AdminRepositoryError::NotFound => {
            AppError::unauthorized(ErrorCode::AdminNotFound, "admin not found")
        }
        other => AppError::internal(other),
    }
}

#[cfg(test)]
mod tests {
    use super::business_access_allowed;
    use axum::http::Method;

    #[test]
    fn default_admin_can_only_read_business_data() {
        let entry = "/api/v1/admin/lexicon/entries/{id}";
        assert!(business_access_allowed(false, false, &Method::GET, entry));
        assert!(business_access_allowed(false, false, &Method::HEAD, entry));
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(!business_access_allowed(false, false, &method, entry));
            assert!(business_access_allowed(true, false, &method, entry));
        }
        assert!(business_access_allowed(
            false,
            false,
            &Method::POST,
            "/api/v1/admin/lexicon/entries/component-targets/search"
        ));
        assert!(!business_access_allowed(
            false,
            false,
            &Method::POST,
            "/api/v1/admin/lexicon/audio-assets/upload-url"
        ));
    }

    #[test]
    fn existing_publication_grant_does_not_grant_editing() {
        let publication = "/api/v1/admin/lexicon/entries/{id}/publications";
        assert!(!business_access_allowed(
            false,
            false,
            &Method::POST,
            publication
        ));
        assert!(business_access_allowed(
            false,
            true,
            &Method::POST,
            publication
        ));
        assert!(!business_access_allowed(
            false,
            true,
            &Method::PUT,
            "/api/v1/admin/lexicon/entries/{id}/steps/forms"
        ));
        assert!(!business_access_allowed(
            false,
            true,
            &Method::POST,
            "/api/v1/admin/lexicon/entries"
        ));
    }
}
