pub mod catalog;
pub mod content;
pub mod handler;
pub mod migration;
pub mod model;
pub mod service;

use std::collections::BTreeSet;

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    admin::{Admin, AdminAuth, authorization::require_active_admin},
    error::{AppError, ErrorCode},
    state::AppState,
};

#[derive(Debug, Clone)]
pub struct AdminAuthorization {
    pub admin_id: Uuid,
    pub is_super_admin: bool,
    pub permission_version: i64,
    pub permissions: BTreeSet<String>,
}

impl AdminAuthorization {
    pub fn has(&self, key: &str) -> bool {
        catalog::definition(key).is_some()
            && (self.is_super_admin || self.permissions.contains(key))
    }

    pub fn require(&self, key: &str) -> Result<(), AppError> {
        if self.has(key) {
            Ok(())
        } else {
            Err(AppError::forbidden(
                ErrorCode::Forbidden,
                "permission required",
            ))
        }
    }

    pub fn require_any_owned_action(&self, keys: &[&str], owner: Uuid) -> Result<(), AppError> {
        if keys
            .iter()
            .any(|key| self.require_owned_action(key, owner).is_ok())
        {
            Ok(())
        } else {
            Err(AppError::forbidden(
                ErrorCode::Forbidden,
                "content editing permission required",
            ))
        }
    }

    pub fn require_owned_action(&self, key: &str, owner: Uuid) -> Result<(), AppError> {
        self.require(key)?;
        let others = match key {
            "words.edit" | "words.associate" => Some("words.edit_others"),
            "sentences.edit" | "sentences.associate" => Some("sentences.edit_others"),
            _ => None,
        };
        if self.is_super_admin
            || self.admin_id == owner
            || others.is_some_and(|permission| self.has(permission))
        {
            Ok(())
        } else {
            Err(AppError::forbidden(
                ErrorCode::Forbidden,
                "content ownership required",
            ))
        }
    }
}

pub async fn load(state: &AppState, auth: &AdminAuth) -> Result<AdminAuthorization, AppError> {
    auth.authorization
        .get_or_try_init(|| async {
            let admin = require_active_admin(state, auth).await?;
            from_admin(&state.pool, &admin).await
        })
        .await
        .cloned()
}

/// Recheck after an external wait before delivering sensitive results; do not use the request snapshot.
pub async fn reload(state: &AppState, auth: &AdminAuth) -> Result<AdminAuthorization, AppError> {
    let admin = crate::admin::authorization::load_active_admin(state, auth).await?;
    from_admin(&state.pool, &admin).await
}

pub async fn from_admin(pool: &PgPool, admin: &Admin) -> Result<AdminAuthorization, AppError> {
    let version = sqlx::query_scalar("SELECT permission_version FROM admins WHERE id = $1")
        .bind(admin.id)
        .fetch_one(pool)
        .await
        .map_err(AppError::internal)?;
    let stored: Vec<String> = sqlx::query_scalar(
        "SELECT permission_key FROM admin_permission_grants WHERE admin_id = $1",
    )
    .bind(admin.id)
    .fetch_all(pool)
    .await
    .map_err(AppError::internal)?;
    Ok(AdminAuthorization {
        admin_id: admin.id,
        is_super_admin: admin.is_super_admin(),
        permission_version: version,
        permissions: if admin.is_super_admin() {
            catalog::all_keys()
        } else {
            catalog::effective_keys(stored)
        },
    })
}

pub async fn lock(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<AdminAuthorization, AppError> {
    let (role, status, must_change, version) = sqlx::query_as::<_, (String, String, bool, i64)>(
        "SELECT role, status, must_change_password, permission_version FROM admins WHERE id = $1 FOR SHARE",
    ).bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?
        .ok_or_else(|| AppError::unauthorized(ErrorCode::AdminNotFound, "admin not found"))?;
    if status != "active" {
        return Err(AppError::forbidden(ErrorCode::AccountDisabled, "forbidden"));
    }
    if must_change {
        return Err(AppError::forbidden(
            ErrorCode::MustChangePassword,
            "password change required",
        ));
    }
    let is_super_admin = role == "super_admin";
    let stored: Vec<String> = sqlx::query_scalar(
        "SELECT permission_key FROM admin_permission_grants WHERE admin_id = $1",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    Ok(AdminAuthorization {
        admin_id: id,
        is_super_admin,
        permission_version: version,
        permissions: if is_super_admin {
            catalog::all_keys()
        } else {
            catalog::effective_keys(stored)
        },
    })
}

pub const LEGACY_PUBLICATION_KEYS: &[&str] = &[
    "words.publish",
    "words.archive",
    "words.restore",
    "words.rollback",
    "sentences.publish",
    "sentences.withdraw",
    "sentences.restore",
    "sentences.rollback",
];

pub fn legacy_publication_allowed(authorization: &AdminAuthorization) -> bool {
    LEGACY_PUBLICATION_KEYS
        .iter()
        .all(|key| authorization.has(key))
}
