use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::catalog::PermissionDefinition;

#[derive(Serialize, ToSchema)]
pub struct PermissionCatalog {
    pub catalog_version: String,
    pub permissions: Vec<PermissionDefinition>,
    pub tags: Vec<PermissionTag>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminPermissions {
    pub admin_id: Uuid,
    pub permission_version: i64,
    pub permissions: Vec<String>,
    pub catalog_version: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewTarget {
    pub admin_id: Uuid,
    pub expected_version: Option<i64>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    pub catalog_version: String,
    pub targets: Vec<PreviewTarget>,
    pub grant: Vec<String>,
    pub revoke: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PermissionChangePreview {
    pub admin_id: Uuid,
    pub expected_version: i64,
    pub before: Vec<String>,
    pub after: Vec<String>,
    pub grant: Vec<String>,
    pub revoke: Vec<String>,
    pub dependency_grants: Vec<String>,
    pub dependency_revocations: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PreviewResponse {
    pub catalog_version: String,
    pub targets: Vec<PermissionChangePreview>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeTarget {
    pub admin_id: Uuid,
    pub expected_version: i64,
    pub grant: Vec<String>,
    pub revoke: Vec<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeRequest {
    pub catalog_version: String,
    pub targets: Vec<ChangeTarget>,
}

#[derive(Serialize, ToSchema)]
pub struct ChangeResponse {
    pub request_id: Uuid,
    pub targets: Vec<AdminPermissions>,
}

#[derive(Clone, Serialize, ToSchema)]
pub struct PermissionTag {
    pub id: Uuid,
    pub name: String,
    pub version: i64,
    pub permissions: Vec<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTagRequest {
    pub name: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateTagRequest {
    pub name: String,
    pub expected_version: i64,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteTagQuery {
    pub expected_version: i64,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TagChangeTarget {
    pub tag_id: Uuid,
    pub expected_version: i64,
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TagChangeRequest {
    pub catalog_version: String,
    pub targets: Vec<TagChangeTarget>,
}

#[derive(Deserialize, IntoParams)]
pub struct TagIdPath {
    pub tag_id: Uuid,
}

#[derive(Deserialize, IntoParams)]
pub struct PermissionKeyPath {
    pub permission_key: String,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PermissionListQuery {
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct GrantedAdmin {
    pub admin_id: Uuid,
    pub display_name: String,
    pub permission_version: i64,
}

#[derive(Serialize, ToSchema)]
pub struct GrantedAdmins {
    pub items: Vec<GrantedAdmin>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub super_admins_are_implicit: bool,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AuditQuery {
    pub admin_id: Option<Uuid>,
    pub permission_key: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct PermissionAudit {
    pub id: Uuid,
    pub actor_admin_id: Uuid,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Uuid,
    pub request_id: Uuid,
    pub metadata: serde_json::Value,
    pub occurred_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use utoipa::OpenApi;

    #[test]
    fn permission_query_parameters_are_query_and_have_correct_requiredness() {
        let spec = serde_json::to_value(crate::openapi::ApiDoc::openapi()).unwrap();
        for (path, method, names, required) in [
            (
                "/api/v1/admin/permission-tags/{tag_id}",
                "delete",
                vec!["expected_version"],
                true,
            ),
            (
                "/api/v1/admin/permissions/{permission_key}/admins",
                "get",
                vec!["page", "page_size"],
                false,
            ),
            (
                "/api/v1/admin/permission-audits",
                "get",
                vec![
                    "admin_id",
                    "permission_key",
                    "since",
                    "until",
                    "page",
                    "page_size",
                ],
                false,
            ),
        ] {
            let parameters = spec["paths"][path][method]["parameters"]
                .as_array()
                .unwrap();
            for name in names {
                let parameter = parameters.iter().find(|p| p["name"] == name).unwrap();
                assert_eq!(parameter["in"], "query", "{path} {name}");
                assert_eq!(parameter["required"], required, "{path} {name}");
            }
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct PermissionAudits {
    pub items: Vec<PermissionAudit>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}
