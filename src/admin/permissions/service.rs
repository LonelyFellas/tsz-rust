use std::collections::{BTreeMap, BTreeSet};

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::{catalog, model::*};
use crate::error::{AppError, ErrorCode};

pub fn conflict(message: &str) -> AppError {
    AppError::conflict(ErrorCode::RevisionConflict, None, message)
}

pub fn check_catalog(version: &str) -> Result<(), AppError> {
    if version == catalog::catalog_version() {
        Ok(())
    } else {
        Err(conflict("permission catalog changed"))
    }
}

fn validate_targets(ids: &[Uuid]) -> Result<(), AppError> {
    if ids.is_empty() || ids.len() > 100 || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(catalog::invalid("select 1 to 100 distinct targets"));
    }
    Ok(())
}

fn selections(
    grant: &[String],
    revoke: &[String],
) -> Result<(BTreeSet<String>, BTreeSet<String>), AppError> {
    let grant = catalog::validate_keys(grant)?;
    let revoke = catalog::validate_keys(revoke)?;
    if !grant.is_disjoint(&revoke) {
        return Err(catalog::invalid("grant and revoke overlap"));
    }
    Ok((grant, revoke))
}

pub fn preview_change(
    id: Uuid,
    version: i64,
    before: &BTreeSet<String>,
    grant: &[String],
    revoke: &[String],
) -> Result<PermissionChangePreview, AppError> {
    let (requested_grant, requested_revoke) = selections(grant, revoke)?;
    let mut expanded_grant = requested_grant.clone();
    catalog::expand_grants(&mut expanded_grant);
    if !expanded_grant.is_disjoint(&requested_revoke) {
        return Err(catalog::invalid(
            "cannot grant a permission while revoking its dependency",
        ));
    }
    let mut after = before.clone();
    after.extend(expanded_grant.iter().cloned());
    for key in &requested_revoke {
        after.remove(key);
    }
    after = catalog::effective_keys(after);
    let actual_grant: BTreeSet<_> = after.difference(before).cloned().collect();
    let actual_revoke: BTreeSet<_> = before.difference(&after).cloned().collect();
    Ok(PermissionChangePreview {
        admin_id: id,
        expected_version: version,
        before: before.iter().cloned().collect(),
        after: after.into_iter().collect(),
        dependency_grants: actual_grant.difference(&requested_grant).cloned().collect(),
        dependency_revocations: actual_revoke
            .difference(&requested_revoke)
            .cloned()
            .collect(),
        grant: actual_grant.into_iter().collect(),
        revoke: actual_revoke.into_iter().collect(),
    })
}

async fn lock_admins(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    targets: &[Uuid],
    write: bool,
) -> Result<BTreeMap<Uuid, i64>, AppError> {
    validate_targets(targets)?;
    let ids: Vec<_> = targets
        .iter()
        .copied()
        .chain([actor])
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let sql = if write {
        "SELECT id, role, status, must_change_password, permission_version FROM admins WHERE id = ANY($1) ORDER BY id FOR UPDATE"
    } else {
        "SELECT id, role, status, must_change_password, permission_version FROM admins WHERE id = ANY($1) ORDER BY id FOR SHARE"
    };
    let rows = sqlx::query_as::<_, (Uuid, String, String, bool, i64)>(sql)
        .bind(&ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    let calling = rows
        .iter()
        .find(|row| row.0 == actor)
        .ok_or_else(|| AppError::unauthorized(ErrorCode::AdminNotFound, "admin not found"))?;
    if calling.2 != "active" {
        return Err(AppError::forbidden(ErrorCode::AccountDisabled, "forbidden"));
    }
    if calling.3 {
        return Err(AppError::forbidden(
            ErrorCode::MustChangePassword,
            "password change required",
        ));
    }
    if calling.1 != "super_admin" {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "super admin required",
        ));
    }
    let mut versions = BTreeMap::new();
    for id in targets {
        let row = rows
            .iter()
            .find(|row| row.0 == *id)
            .ok_or_else(|| AppError::not_found("target admin not found"))?;
        if row.1 != "admin" {
            return Err(catalog::invalid("super admin permissions are intrinsic"));
        }
        versions.insert(*id, row.4);
    }
    Ok(versions)
}

async fn read_grants(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<BTreeMap<Uuid, BTreeSet<String>>, AppError> {
    let rows = sqlx::query_as::<_, (Uuid, String)>("SELECT admin_id, permission_key FROM admin_permission_grants WHERE admin_id = ANY($1) ORDER BY admin_id, permission_key")
        .bind(ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let mut result: BTreeMap<_, BTreeSet<_>> =
        ids.iter().map(|id| (*id, BTreeSet::new())).collect();
    for (id, key) in rows {
        result.entry(id).or_default().insert(key);
    }
    for keys in result.values_mut() {
        *keys = catalog::effective_keys(std::mem::take(keys));
    }
    Ok(result)
}

pub async fn snapshot(
    pool: &PgPool,
    actor: Uuid,
    target: Uuid,
) -> Result<AdminPermissions, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let versions = lock_admins(&mut tx, actor, &[target], false).await?;
    let grants = read_grants(&mut tx, &[target]).await?;
    Ok(AdminPermissions {
        admin_id: target,
        permission_version: versions[&target],
        permissions: grants[&target].iter().cloned().collect(),
        catalog_version: catalog::catalog_version(),
    })
}

pub async fn preview(
    pool: &PgPool,
    actor: Uuid,
    input: PreviewRequest,
) -> Result<PreviewResponse, AppError> {
    check_catalog(&input.catalog_version)?;
    selections(&input.grant, &input.revoke)?;
    let ids: Vec<_> = input.targets.iter().map(|target| target.admin_id).collect();
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let versions = lock_admins(&mut tx, actor, &ids, false).await?;
    let grants = read_grants(&mut tx, &ids).await?;
    let mut targets = Vec::new();
    for target in input.targets {
        let version = versions[&target.admin_id];
        if target
            .expected_version
            .is_some_and(|expected| expected != version)
        {
            return Err(conflict("admin permissions changed"));
        }
        targets.push(preview_change(
            target.admin_id,
            version,
            &grants[&target.admin_id],
            &input.grant,
            &input.revoke,
        )?);
    }
    Ok(PreviewResponse {
        catalog_version: input.catalog_version,
        targets,
    })
}

pub async fn apply(
    pool: &PgPool,
    actor: Uuid,
    request_id: Uuid,
    input: ChangeRequest,
) -> Result<ChangeResponse, AppError> {
    check_catalog(&input.catalog_version)?;
    let ids: Vec<_> = input.targets.iter().map(|target| target.admin_id).collect();
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let versions = lock_admins(&mut tx, actor, &ids, true).await?;
    let grants = read_grants(&mut tx, &ids).await?;
    let mut changes = Vec::new();
    for target in input.targets {
        let version = versions[&target.admin_id];
        if target.expected_version != version {
            return Err(conflict("admin permissions changed"));
        }
        let (grant, revoke) = selections(&target.grant, &target.revoke)?;
        let mut after = grants[&target.admin_id].clone();
        after.extend(grant);
        for key in revoke {
            after.remove(&key);
        }
        catalog::validate_closed(&after)?;
        let before = &grants[&target.admin_id];
        changes.push((target.admin_id, version, before.clone(), after));
    }
    let mut targets = Vec::new();
    for (id, mut version, before, after) in changes {
        let grant: Vec<_> = after.difference(&before).cloned().collect();
        let revoke: Vec<_> = before.difference(&after).cloned().collect();
        if before != after {
            sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id = $1 AND permission_key = ANY($2)")
                .bind(id).bind(&revoke).execute(&mut *tx).await.map_err(AppError::internal)?;
            if !grant.is_empty() {
                sqlx::query("INSERT INTO admin_permission_grants (admin_id, permission_key, granted_by) SELECT $1, unnest($2::text[]), $3 ON CONFLICT DO NOTHING")
                    .bind(id).bind(&grant).bind(actor).execute(&mut *tx).await.map_err(AppError::internal)?;
            }
            version += 1;
            sqlx::query("UPDATE admins SET permission_version = $2 WHERE id = $1")
                .bind(id)
                .bind(version)
                .execute(&mut *tx)
                .await
                .map_err(AppError::internal)?;
            audit(&mut tx, actor, request_id, "admin.permissions.change", "admin", id, serde_json::json!({
                "before": before, "after": after, "grant": grant, "revoke": revoke,
                "before_version": version - 1, "permission_version": version, "catalog_version": input.catalog_version,
            })).await?;
        }
        targets.push(AdminPermissions {
            admin_id: id,
            permission_version: version,
            permissions: after.into_iter().collect(),
            catalog_version: input.catalog_version.clone(),
        });
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(ChangeResponse {
        request_id,
        targets,
    })
}

pub async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    request_id: Uuid,
    action: &str,
    resource_type: &str,
    id: Uuid,
    metadata: serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO audit.admin_actions (id, actor_admin_id, action, resource_type, resource_id, request_id, metadata) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(Uuid::now_v7()).bind(actor).bind(action).bind(resource_type).bind(id).bind(request_id).bind(metadata)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}

pub async fn tags(pool: &PgPool) -> Result<Vec<PermissionTag>, AppError> {
    let rows = sqlx::query_as::<_, (Uuid, String, String, i64, Vec<String>)>(
        "SELECT t.id, t.name, t.color, t.version, COALESCE(array_agg(i.permission_key ORDER BY i.permission_key) FILTER (WHERE i.permission_key IS NOT NULL), ARRAY[]::text[]) FROM permission_tags t LEFT JOIN permission_tag_items i ON i.tag_id = t.id GROUP BY t.id ORDER BY lower(t.name), t.id",
    ).fetch_all(pool).await.map_err(AppError::internal)?;
    Ok(rows
        .into_iter()
        .map(|(id, name, color, version, permissions)| PermissionTag {
            id,
            name,
            color,
            version,
            permissions,
        })
        .collect())
}

fn tag_name(name: &str) -> Result<String, AppError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 50 {
        return Err(catalog::invalid("tag name must contain 1 to 50 characters"));
    }
    Ok(name.to_owned())
}

fn tag_color(color: &str) -> Result<String, AppError> {
    match color {
        "default" | "blue" | "cyan" | "green" | "gold" | "orange" | "red" | "purple" => {
            Ok(color.to_owned())
        }
        _ if color.len() == 7
            && color.as_bytes()[0] == b'#'
            && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit) =>
        {
            Ok(color.to_ascii_uppercase())
        }
        _ => Err(catalog::invalid("invalid tag color")),
    }
}

fn map_tag_error(error: sqlx::Error) -> AppError {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        conflict("tag name already exists")
    } else {
        AppError::internal(error)
    }
}

async fn lock_tag_actor(tx: &mut Transaction<'_, Postgres>, actor: Uuid) -> Result<(), AppError> {
    let authorization = super::lock(tx, actor).await?;
    if authorization.is_super_admin {
        Ok(())
    } else {
        Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "super admin required",
        ))
    }
}

pub async fn create_tag(
    pool: &PgPool,
    actor: Uuid,
    request_id: Uuid,
    name: &str,
    color: Option<&str>,
) -> Result<PermissionTag, AppError> {
    let name = tag_name(name)?;
    let color = tag_color(color.unwrap_or("default"))?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_tag_actor(&mut tx, actor).await?;
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO permission_tags (id, name, color) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(&name)
        .bind(&color)
        .execute(&mut *tx)
        .await
        .map_err(map_tag_error)?;
    audit(
        &mut tx,
        actor,
        request_id,
        "admin.permission_tags.create",
        "permission_tag",
        id,
        serde_json::json!({"after": name, "color": color, "version": 0}),
    )
    .await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(PermissionTag {
        id,
        name,
        color,
        version: 0,
        permissions: vec![],
    })
}

pub async fn update_tag(
    pool: &PgPool,
    actor: Uuid,
    request_id: Uuid,
    id: Uuid,
    expected: i64,
    name: Option<&str>,
    color: Option<&str>,
) -> Result<(), AppError> {
    let name = name.map(tag_name).transpose()?;
    let color = color.map(tag_color).transpose()?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_tag_actor(&mut tx, actor).await?;
    let (before_name, before_color, version) = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT name, color, version FROM permission_tags WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("tag not found"))?;
    if expected != version {
        return Err(conflict("tag changed"));
    }
    if name.as_ref().is_some_and(|name| name == &before_name)
        && color.as_deref().unwrap_or(&before_color) == before_color
    {
        return Ok(());
    }
    let items: Vec<String> = sqlx::query_scalar(
        "SELECT permission_key FROM permission_tag_items WHERE tag_id = $1 ORDER BY permission_key",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let action = if let Some(name) = &name {
        sqlx::query("UPDATE permission_tags SET name = $2, color = $3, version = version + 1, updated_at = now() WHERE id = $1")
            .bind(id).bind(name).bind(color.as_deref().unwrap_or(&before_color)).execute(&mut *tx).await.map_err(map_tag_error)?;
        if color.as_ref().is_some_and(|color| color != &before_color) {
            "admin.permission_tags.update"
        } else {
            "admin.permission_tags.rename"
        }
    } else {
        sqlx::query("DELETE FROM permission_tags WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        "admin.permission_tags.delete"
    };
    audit(&mut tx, actor, request_id, action, "permission_tag", id, serde_json::json!({"before": {"name": before_name, "color": before_color, "permissions": items, "version": version}, "after": name, "color_after": name.as_ref().map(|_| color.as_deref().unwrap_or(&before_color)), "version": version + 1})).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(())
}

pub async fn change_tags(
    pool: &PgPool,
    actor: Uuid,
    request_id: Uuid,
    input: TagChangeRequest,
) -> Result<Vec<PermissionTag>, AppError> {
    check_catalog(&input.catalog_version)?;
    let ids: Vec<_> = input.targets.iter().map(|target| target.tag_id).collect();
    validate_targets(&ids)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_tag_actor(&mut tx, actor).await?;
    let rows = sqlx::query_as::<_, (Uuid, String, String, i64)>(
        "SELECT id, name, color, version FROM permission_tags WHERE id = ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let mut changes = Vec::new();
    for target in input.targets {
        let row = rows
            .iter()
            .find(|row| row.0 == target.tag_id)
            .ok_or_else(|| AppError::not_found("tag not found"))?;
        if target.expected_version != row.3 {
            return Err(conflict("tag changed"));
        }
        let (add, remove) = selections(&target.add, &target.remove)?;
        let stored: Vec<String> = sqlx::query_scalar("SELECT permission_key FROM permission_tag_items WHERE tag_id = $1 ORDER BY permission_key")
            .bind(target.tag_id).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
        let before: BTreeSet<_> = stored.into_iter().collect();
        let mut after = before.clone();
        after.extend(add);
        for key in remove {
            after.remove(&key);
        }
        changes.push((
            target.tag_id,
            row.1.clone(),
            row.2.clone(),
            row.3,
            before,
            after,
        ));
    }
    let mut result = Vec::new();
    for (id, name, color, mut version, before, after) in changes {
        if before != after {
            let remove: Vec<_> = before.difference(&after).cloned().collect();
            sqlx::query(
                "DELETE FROM permission_tag_items WHERE tag_id = $1 AND permission_key = ANY($2)",
            )
            .bind(id)
            .bind(remove)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
            for key in after.difference(&before) {
                sqlx::query(
                    "INSERT INTO permission_tag_items (tag_id, permission_key) VALUES ($1, $2)",
                )
                .bind(id)
                .bind(key)
                .execute(&mut *tx)
                .await
                .map_err(AppError::internal)?;
            }
            version += 1;
            sqlx::query(
                "UPDATE permission_tags SET version = $2, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(version)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
            audit(&mut tx, actor, request_id, "admin.permission_tags.items", "permission_tag", id, serde_json::json!({"before": before, "after": after, "version": version, "catalog_version": input.catalog_version})).await?;
        }
        result.push(PermissionTag {
            id,
            name,
            color,
            version,
            permissions: after.into_iter().collect(),
        });
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_expands_dependencies_and_reverse_dependencies() {
        let id = Uuid::nil();
        let first =
            preview_change(id, 0, &BTreeSet::new(), &["words.edit_others".into()], &[]).unwrap();
        assert_eq!(
            first.grant,
            ["words.access", "words.edit", "words.edit_others"]
        );
        assert_eq!(first.dependency_grants, ["words.access", "words.edit"]);
        let before = first.after.into_iter().collect();
        let next = preview_change(id, 1, &before, &[], &["words.edit".into()]).unwrap();
        assert_eq!(next.after, ["words.access"]);
        assert_eq!(next.dependency_revocations, ["words.edit_others"]);
        assert!(
            preview_change(
                id,
                1,
                &before,
                &["words.edit_others".into()],
                &["words.access".into()]
            )
            .is_err()
        );
    }
}
