use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::{LEGACY_PUBLICATION_KEYS, catalog, model::AdminPermissions};
use crate::error::AppError;

#[derive(Serialize)]
pub struct MigrationTargetPreview {
    pub admin_id: Uuid,
    pub display_name: String,
    pub status: String,
    pub legacy_can_publish_lexicon: bool,
    pub current: AdminPermissions,
    pub proposed_permissions: Vec<String>,
    pub grant: Vec<String>,
    pub revoke: Vec<String>,
    pub scope_changes: Vec<&'static str>,
    pub migration_candidate: bool,
    pub approval_required: bool,
}

#[derive(Serialize)]
pub struct MigrationPreview {
    pub catalog_version: String,
    pub legacy_backend_version: &'static str,
    pub targets: Vec<MigrationTargetPreview>,
    pub applied: bool,
}

pub async fn preview(pool: &PgPool) -> Result<MigrationPreview, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let rows = sqlx::query_as::<_, (Uuid, String, String, bool, i64)>(
        "SELECT id, display_name, status, can_publish_lexicon, permission_version FROM admins WHERE role = 'admin' ORDER BY id",
    ).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let mut targets = Vec::new();
    for (id, name, status, publication, version) in rows {
        let stored: Vec<String> = sqlx::query_scalar("SELECT permission_key FROM admin_permission_grants WHERE admin_id = $1 ORDER BY permission_key")
            .bind(id).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
        let current = catalog::effective_keys(stored);
        let mut proposed = catalog::effective_keys([
            "words.access".into(),
            "sentences.access".into(),
            "users.access".into(),
            "users.read_sensitive".into(),
        ]);
        if publication {
            proposed.extend(LEGACY_PUBLICATION_KEYS.iter().map(|key| key.to_string()));
            catalog::expand_grants(&mut proposed);
        }
        let migration_candidate = version == 0 && current.is_empty();
        if !migration_candidate {
            proposed = current.clone();
        }
        let scope_changes = if publication && migration_candidate {
            vec![
                "archive/restore previously allowed published content of other administrators; the proposed keys are owner-only",
            ]
        } else {
            vec![]
        };
        targets.push(MigrationTargetPreview {
            admin_id: id,
            display_name: name,
            status,
            legacy_can_publish_lexicon: publication,
            grant: proposed.difference(&current).cloned().collect(),
            revoke: current.difference(&proposed).cloned().collect(),
            current: AdminPermissions {
                admin_id: id,
                permission_version: version,
                permissions: current.into_iter().collect(),
                catalog_version: catalog::catalog_version(),
            },
            proposed_permissions: proposed.into_iter().collect(),
            scope_changes,
            migration_candidate,
            approval_required: true,
        });
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(MigrationPreview {
        catalog_version: catalog::catalog_version(),
        legacy_backend_version: "cc1723121a171020d154e0cdc13908fe34c74159",
        targets,
        applied: false,
    })
}
