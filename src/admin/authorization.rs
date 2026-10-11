use axum::{
    extract::{FromRequestParts, MatchedPath, Request, State},
    middleware::Next,
    response::Response,
};

use crate::{
    admin::{Admin, AdminAuth, AdminRepository, AdminRepositoryError, permissions},
    error::{AppError, ErrorCode},
    state::AppState,
};

pub(crate) async fn require_active_admin(
    state: &AppState,
    auth: &AdminAuth,
) -> Result<Admin, AppError> {
    auth.active_admin
        .get_or_try_init(|| load_active_admin(state, auth))
        .await
        .cloned()
}

pub(crate) async fn load_active_admin(
    state: &AppState,
    auth: &AdminAuth,
) -> Result<Admin, AppError> {
    let admin = AdminRepository::new(state.pool.clone())
        .get_by_id(&auth.subject)
        .await
        .map_err(map_admin_error)?;
    if admin.security_version != auth.security_version {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
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
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "super admin required",
        ));
    }
    Ok(admin)
}

#[derive(Debug, Clone, Copy)]
pub enum RoutePolicy {
    SessionFlow,
    ActiveSession,
    SuperAdmin,
    All(&'static [&'static str]),
    Any(&'static [&'static str]),
}

pub const ROUTE_POLICIES: &[(&str, &str, RoutePolicy)] = &[
    ("GET", "/wordlists", RoutePolicy::All(&["wordlists.access"])),
    (
        "GET",
        "/wordlists/{id}/review-requests",
        RoutePolicy::All(&["wordlists.access"]),
    ),
    (
        "GET",
        "/wordlists/{id}/review-requests/{request_id}/items",
        RoutePolicy::All(&["wordlists.access"]),
    ),
    (
        "POST",
        "/wordlists/{id}/review-requests/{request_id}/decision",
        RoutePolicy::All(&["wordlists.review"]),
    ),
    (
        "POST",
        "/wordlists/{id}/withdraw",
        RoutePolicy::All(&["wordlists.withdraw"]),
    ),
    (
        "GET",
        "/coins/accounts",
        RoutePolicy::All(&["coins.access"]),
    ),
    (
        "GET",
        "/coins/accounts/{owner_type}/{owner_id}/entries",
        RoutePolicy::All(&["coins.access"]),
    ),
    (
        "GET",
        "/coins/operations",
        RoutePolicy::All(&["coins.access"]),
    ),
    (
        "POST",
        "/coins/manual-credits",
        RoutePolicy::All(&["coins.credit"]),
    ),
    (
        "POST",
        "/coins/manual-credits/{operation_id}/reversal",
        RoutePolicy::All(&["coins.reverse"]),
    ),
    ("POST", "/auth/login", RoutePolicy::SessionFlow),
    ("POST", "/auth/login-code", RoutePolicy::SessionFlow),
    ("POST", "/auth/refresh", RoutePolicy::SessionFlow),
    ("POST", "/auth/logout", RoutePolicy::SessionFlow),
    ("POST", "/auth/logout-all", RoutePolicy::SessionFlow),
    ("POST", "/auth/change-password", RoutePolicy::SessionFlow),
    ("GET", "/profile", RoutePolicy::ActiveSession),
    ("GET", "/me/coins/wallet", RoutePolicy::ActiveSession),
    ("GET", "/me/coins/entries", RoutePolicy::ActiveSession),
    ("PATCH", "/profile/preferences", RoutePolicy::ActiveSession),
    ("GET", "/admins", RoutePolicy::SuperAdmin),
    ("POST", "/admins", RoutePolicy::SuperAdmin),
    ("POST", "/admins/create-code", RoutePolicy::SuperAdmin),
    ("PATCH", "/admins/{admin_id}", RoutePolicy::SuperAdmin),
    (
        "PATCH",
        "/admins/{admin_id}/status",
        RoutePolicy::SuperAdmin,
    ),
    (
        "POST",
        "/admins/{admin_id}/reset-password",
        RoutePolicy::SuperAdmin,
    ),
    (
        "PATCH",
        "/admins/{admin_id}/lexicon-publication-permission",
        RoutePolicy::SuperAdmin,
    ),
    ("GET", "/permissions", RoutePolicy::SuperAdmin),
    (
        "GET",
        "/admins/{admin_id}/permissions",
        RoutePolicy::SuperAdmin,
    ),
    (
        "GET",
        "/permissions/{permission_key}/admins",
        RoutePolicy::SuperAdmin,
    ),
    (
        "POST",
        "/permission-changes/preview",
        RoutePolicy::SuperAdmin,
    ),
    ("POST", "/permission-changes", RoutePolicy::SuperAdmin),
    ("GET", "/permission-tags", RoutePolicy::SuperAdmin),
    ("POST", "/permission-tags", RoutePolicy::SuperAdmin),
    (
        "PATCH",
        "/permission-tags/{tag_id}",
        RoutePolicy::SuperAdmin,
    ),
    (
        "DELETE",
        "/permission-tags/{tag_id}",
        RoutePolicy::SuperAdmin,
    ),
    ("POST", "/permission-tag-changes", RoutePolicy::SuperAdmin),
    ("GET", "/permission-audits", RoutePolicy::SuperAdmin),
    ("GET", "/users", RoutePolicy::All(&["users.access"])),
    ("GET", "/users/{id}", RoutePolicy::All(&["users.access"])),
    ("PATCH", "/users/{id}", RoutePolicy::All(&["users.edit"])),
    (
        "PATCH",
        "/users/{id}/status",
        RoutePolicy::All(&["users.set_status"]),
    ),
    (
        "GET",
        "/teacher-applications",
        RoutePolicy::All(&["teacherapply.access"]),
    ),
    (
        "GET",
        "/teacher-applications/{id}",
        RoutePolicy::All(&["teacherapply.access"]),
    ),
    (
        "POST",
        "/teacher-applications/{id}/review",
        RoutePolicy::All(&["teacherapply.review"]),
    ),
    (
        "DELETE",
        "/users/{id}/teacher-certification",
        RoutePolicy::All(&["teacherapply.revoke"]),
    ),
    (
        "GET",
        "/teacher-certification/files/{id}",
        RoutePolicy::All(&["teacherapply.read_sensitive"]),
    ),
    (
        "GET",
        "/settings/parts-of-speech",
        RoutePolicy::All(&["lexicon_settings.access"]),
    ),
    (
        "GET",
        "/settings/parts-of-speech/catalog",
        RoutePolicy::Any(&[
            "lexicon_settings.access",
            "words.access",
            "sentences.access",
        ]),
    ),
    (
        "GET",
        "/settings/parts-of-speech/{id}/sub-parts",
        RoutePolicy::All(&["lexicon_settings.access"]),
    ),
    (
        "GET",
        "/settings/form-types",
        RoutePolicy::Any(&[
            "lexicon_settings.access",
            "words.create",
            "words.edit",
            "words.associate",
            "sentences.create",
            "sentences.edit",
            "sentences.associate",
        ]),
    ),
    (
        "POST",
        "/settings/parts-of-speech",
        RoutePolicy::All(&["lexicon_settings.edit"]),
    ),
    (
        "PATCH",
        "/settings/parts-of-speech/{id}",
        RoutePolicy::All(&["lexicon_settings.edit"]),
    ),
    (
        "DELETE",
        "/settings/parts-of-speech/{id}",
        RoutePolicy::All(&["lexicon_settings.edit"]),
    ),
    (
        "POST",
        "/settings/parts-of-speech/{id}/sub-parts",
        RoutePolicy::All(&["lexicon_settings.edit"]),
    ),
    (
        "PATCH",
        "/settings/parts-of-speech/{id}/sub-parts/{sub_id}",
        RoutePolicy::All(&["lexicon_settings.edit"]),
    ),
    (
        "DELETE",
        "/settings/parts-of-speech/{id}/sub-parts/{sub_id}",
        RoutePolicy::All(&["lexicon_settings.edit"]),
    ),
    (
        "POST",
        "/settings/form-types",
        RoutePolicy::All(&["lexicon_settings.edit"]),
    ),
    (
        "PATCH",
        "/settings/form-types/{id}",
        RoutePolicy::All(&["lexicon_settings.edit"]),
    ),
    (
        "DELETE",
        "/settings/form-types/{id}",
        RoutePolicy::All(&["lexicon_settings.edit"]),
    ),
    (
        "GET",
        "/lexicon/entries",
        RoutePolicy::All(&["words.access"]),
    ),
    (
        "GET",
        "/lexicon/entries/stats",
        RoutePolicy::All(&["words.access"]),
    ),
    (
        "GET",
        "/lexicon/entries/related-search",
        RoutePolicy::All(&["words.access"]),
    ),
    (
        "GET",
        "/lexicon/entries/{id}",
        RoutePolicy::All(&["words.access"]),
    ),
    (
        "GET",
        "/lexicon/entries/{id}/inbound-references",
        RoutePolicy::All(&["words.access"]),
    ),
    (
        "GET",
        "/lexicon/entries/{id}/publications",
        RoutePolicy::All(&["words.access"]),
    ),
    (
        "GET",
        "/lexicon/entries/{id}/publications/{publication_id}",
        RoutePolicy::All(&["words.access"]),
    ),
    (
        "GET",
        "/lexicon/surface-match-snapshots/{snapshot_id}",
        RoutePolicy::All(&["words.access"]),
    ),
    (
        "POST",
        "/lexicon/detections",
        RoutePolicy::Any(&["words.create", "words.detect"]),
    ),
    (
        "POST",
        "/lexicon/entries/component-targets/search",
        RoutePolicy::All(&["words.access"]),
    ),
    (
        "POST",
        "/lexicon/entries/{id}/steps/forms/impact",
        RoutePolicy::Any(&["words.edit", "words.publish", "words.validate"]),
    ),
    (
        "POST",
        "/lexicon/entries/{id}/validate",
        RoutePolicy::Any(&["words.edit", "words.publish", "words.validate"]),
    ),
    (
        "POST",
        "/lexicon/entries",
        RoutePolicy::All(&["words.create"]),
    ),
    (
        "PUT",
        "/lexicon/entries/{id}/steps/forms",
        RoutePolicy::All(&["words.edit"]),
    ),
    (
        "PUT",
        "/lexicon/entries/{id}/steps/meanings",
        RoutePolicy::Any(&["words.edit", "words.associate"]),
    ),
    (
        "PATCH",
        "/lexicon/entries/{id}/annotation",
        RoutePolicy::All(&["words.edit"]),
    ),
    (
        "PUT",
        "/lexicon/entries/{entry_id}/sentences/{sentence_id}/visibility",
        RoutePolicy::All(&["words.edit"]),
    ),
    (
        "POST",
        "/lexicon/entries/{id}/publications",
        RoutePolicy::All(&["words.publish"]),
    ),
    (
        "POST",
        "/lexicon/entries/publications/batch",
        RoutePolicy::Any(&["words.publish", "sentences.publish"]),
    ),
    (
        "POST",
        "/lexicon/entries/{id}/publications/{publication_id}/rollback",
        RoutePolicy::All(&["words.rollback"]),
    ),
    (
        "POST",
        "/lexicon/entries/{id}/archive",
        RoutePolicy::All(&["words.archive"]),
    ),
    (
        "POST",
        "/lexicon/entries/archive-batch",
        RoutePolicy::All(&["words.archive"]),
    ),
    (
        "POST",
        "/lexicon/entries/{id}/restore",
        RoutePolicy::All(&["words.restore"]),
    ),
    (
        "POST",
        "/lexicon/entries/restore-batch",
        RoutePolicy::All(&["words.restore"]),
    ),
    ("DELETE", "/lexicon/entries/{id}", RoutePolicy::SuperAdmin),
    (
        "POST",
        "/lexicon/entries/delete-batch",
        RoutePolicy::SuperAdmin,
    ),
    (
        "GET",
        "/lexicon/sentences",
        RoutePolicy::All(&["sentences.access"]),
    ),
    (
        "GET",
        "/lexicon/sentences/{id}",
        RoutePolicy::All(&["sentences.access"]),
    ),
    (
        "GET",
        "/lexicon/sentences/{id}/publications",
        RoutePolicy::All(&["sentences.access"]),
    ),
    (
        "GET",
        "/lexicon/sentences/{id}/publications/{publication_id}",
        RoutePolicy::All(&["sentences.access"]),
    ),
    (
        "GET",
        "/lexicon/sentences/{id}/withdrawal-impact",
        RoutePolicy::All(&["sentences.access"]),
    ),
    (
        "GET",
        "/lexicon/sentences/targets",
        RoutePolicy::All(&["sentences.access", "words.access"]),
    ),
    (
        "POST",
        "/lexicon/sentences",
        RoutePolicy::All(&["sentences.create"]),
    ),
    (
        "PUT",
        "/lexicon/sentences/{id}",
        RoutePolicy::Any(&["sentences.edit", "sentences.associate"]),
    ),
    ("DELETE", "/lexicon/sentences/{id}", RoutePolicy::SuperAdmin),
    (
        "POST",
        "/lexicon/sentences/{id}/publications",
        RoutePolicy::All(&["sentences.publish"]),
    ),
    (
        "POST",
        "/lexicon/sentences/{id}/publications/{publication_id}/rollback",
        RoutePolicy::All(&["sentences.rollback"]),
    ),
    (
        "POST",
        "/lexicon/sentences/{id}/withdraw",
        RoutePolicy::All(&["sentences.withdraw"]),
    ),
    (
        "POST",
        "/lexicon/sentences/{id}/restore",
        RoutePolicy::All(&["sentences.restore"]),
    ),
    (
        "GET",
        "/speech/voices",
        RoutePolicy::Any(&["words.access", "sentences.access"]),
    ),
    (
        "POST",
        "/speech/previews",
        RoutePolicy::All(&["speech.generate"]),
    ),
    (
        "POST",
        "/lexicon/audio-assets/upload-url",
        RoutePolicy::Any(&[
            "words.create",
            "words.edit",
            "sentences.create",
            "sentences.edit",
        ]),
    ),
    (
        "POST",
        "/lexicon/audio-assets",
        RoutePolicy::Any(&[
            "words.create",
            "words.edit",
            "sentences.create",
            "sentences.edit",
        ]),
    ),
    (
        "GET",
        "/lexicon/audio-assets/{id}/url",
        RoutePolicy::Any(&["words.access", "sentences.access"]),
    ),
];

pub fn route_policy(method: &str, path: &str) -> Option<RoutePolicy> {
    let method = if method == "HEAD" { "GET" } else { method };
    let path = path.strip_prefix("/api/v1/admin")?;
    ROUTE_POLICIES
        .iter()
        .find(|(verb, route, _)| *verb == method && *route == path)
        .map(|(_, _, policy)| *policy)
}

pub(crate) async fn enforce_business_access(
    State(state): State<AppState>,
    path: MatchedPath,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    if !path.as_str().starts_with("/api/v1/admin/") {
        return Ok(next.run(request).await);
    }
    let policy = route_policy(request.method().as_str(), path.as_str())
        .ok_or_else(|| AppError::forbidden(ErrorCode::Forbidden, "unregistered admin route"))?;
    if matches!(policy, RoutePolicy::SessionFlow) {
        return Ok(next.run(request).await);
    }
    let (mut parts, body) = request.into_parts();
    let auth = AdminAuth::from_request_parts(&mut parts, &state).await?;
    let authorization = permissions::load(&state, &auth).await?;
    match policy {
        RoutePolicy::SuperAdmin if !authorization.is_super_admin => {
            return Err(AppError::forbidden(
                ErrorCode::Forbidden,
                "super admin required",
            ));
        }
        RoutePolicy::All(keys) => {
            for key in keys {
                authorization.require(key)?;
            }
        }
        RoutePolicy::Any(keys) if !keys.iter().any(|key| authorization.has(key)) => {
            return Err(AppError::forbidden(
                ErrorCode::Forbidden,
                "permission required",
            ));
        }
        _ => {}
    }
    parts.extensions.insert(authorization);
    Ok(next.run(Request::from_parts(parts, body)).await)
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
    use super::*;
    use std::collections::BTreeSet;
    use utoipa::OpenApi;

    #[test]
    fn actual_registered_routes_and_policies_match() {
        let sources = [
            (include_str!("router.rs"), ""),
            (include_str!("accounts/router.rs"), "/admins"),
            (include_str!("auth/router.rs"), "/auth"),
            (include_str!("permissions/handler.rs"), ""),
            (
                include_str!("../catalog/router.rs"),
                "/settings/parts-of-speech",
            ),
            (
                include_str!("../catalog/form_types.rs"),
                "/settings/form-types",
            ),
            (include_str!("../lexicon/router.rs"), "/lexicon"),
            (include_str!("../speech/preview/router.rs"), "/speech"),
            (include_str!("../teacher_certification/mod.rs"), ""),
        ];
        let mut registered = BTreeSet::new();
        for (source, mount) in sources {
            let mut remaining = source;
            while let Some(start) = remaining.find(".route(") {
                remaining = &remaining[start + 7..];
                let mut depth = 1;
                let mut quoted = false;
                let mut escaped = false;
                let end = remaining
                    .char_indices()
                    .find_map(|(index, character)| {
                        if escaped {
                            escaped = false;
                            return None;
                        }
                        if quoted && character == '\\' {
                            escaped = true;
                            return None;
                        }
                        if character == '"' {
                            quoted = !quoted;
                        }
                        if !quoted {
                            if character == '(' {
                                depth += 1;
                            }
                            if character == ')' {
                                depth -= 1;
                            }
                        }
                        (depth == 0).then_some(index)
                    })
                    .expect("balanced route registration");
                let route = &remaining[..end];
                let path = route.split('"').nth(1).expect("literal route path");
                let path = if let Some(relative) = path.strip_prefix("/api/v1/admin") {
                    relative.to_owned()
                } else if path.starts_with("/api/") {
                    remaining = &remaining[end + 1..];
                    continue;
                } else {
                    format!("{mount}{}", if path == "/" { "" } else { path })
                };
                for method in [
                    "get", "post", "put", "patch", "delete", "head", "options", "trace",
                ] {
                    if route.contains(&format!("{method}(")) {
                        registered.insert((method.to_uppercase(), path.clone()));
                    }
                }
                remaining = &remaining[end + 1..];
            }
        }
        let expected: BTreeSet<_> = ROUTE_POLICIES
            .iter()
            .map(|(method, path, _)| (method.to_string(), path.to_string()))
            .collect();
        assert_eq!(
            registered, expected,
            "registered admin routes need an explicit policy and OpenAPI operation"
        );
    }

    #[test]
    fn every_admin_openapi_operation_has_an_explicit_policy() {
        let spec = serde_json::to_value(crate::openapi::ApiDoc::openapi()).unwrap();
        let mut actual = BTreeSet::new();
        for (path, item) in spec["paths"].as_object().unwrap() {
            if path.starts_with("/api/v1/admin/") {
                for method in item.as_object().unwrap().keys() {
                    if ["get", "post", "put", "patch", "delete"].contains(&method.as_str()) {
                        assert!(
                            route_policy(&method.to_uppercase(), path).is_some(),
                            "missing policy: {method} {path}"
                        );
                        actual.insert((
                            method.to_uppercase(),
                            path.trim_start_matches("/api/v1/admin").to_owned(),
                        ));
                    }
                }
            }
        }
        let expected: BTreeSet<_> = ROUTE_POLICIES
            .iter()
            .map(|(method, path, _)| (method.to_string(), path.to_string()))
            .collect();
        assert_eq!(expected.len(), ROUTE_POLICIES.len(), "duplicate policies");
        assert_eq!(actual, expected);
        for (_, _, policy) in ROUTE_POLICIES {
            if let RoutePolicy::All(keys) | RoutePolicy::Any(keys) = policy {
                for key in *keys {
                    assert!(
                        permissions::catalog::definition(key).is_some(),
                        "unknown route permission: {key}"
                    );
                }
            }
        }
        assert!(route_policy("GET", "/api/v1/admin/new-unregistered-route").is_none());
        assert!(route_policy("HEAD", "/api/v1/admin/lexicon/entries").is_some());
    }
}
