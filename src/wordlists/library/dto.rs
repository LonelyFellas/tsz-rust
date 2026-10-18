use super::super::dto::Wordlist;
use crate::api::PaginationMeta;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySpace {
    Owned,
    Favorites,
}
impl LibrarySpace {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Favorites => "favorites",
        }
    }
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct LibraryQuery {
    pub parent_id: Option<Uuid>,
    pub wordlist_id: Option<Uuid>,
    pub trash: Option<bool>,
    pub folders_only: Option<bool>,
    pub q: Option<String>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}
#[derive(Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryCrumb {
    pub id: Uuid,
    pub name: String,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryItem {
    pub id: Uuid,
    #[schema(required = true)]
    pub parent_id: Option<Uuid>,
    #[schema(required = true)]
    pub wordlist_id: Option<Uuid>,
    pub name: String,
    pub revision: i64,
    #[schema(required = true)]
    pub trashed_at: Option<DateTime<Utc>>,
    pub path: Vec<LibraryCrumb>,
    #[schema(required = true)]
    pub wordlist: Option<Wordlist>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryPage {
    pub version: String,
    pub ancestors: Vec<LibraryCrumb>,
    pub items: Vec<LibraryItem>,
    pub pagination: PaginationMeta,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryFolderInput {
    pub expected_version: String,
    pub parent_id: Option<Uuid>,
    #[schema(min_length = 1, max_length = 100)]
    pub name: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryRename {
    pub expected_version: String,
    #[schema(min_length = 1, max_length = 100)]
    pub name: String,
}
#[derive(Clone, Copy, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibraryAction {
    Move,
    Trash,
    Restore,
    Purge,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryOperation {
    pub expected_version: String,
    pub action: LibraryAction,
    #[schema(max_items = 100)]
    pub ids: Vec<Uuid>,
    pub target_id: Option<Uuid>,
    /// Restore at the original location when true; otherwise target_id (null = root).
    #[serde(default)]
    pub original_location: bool,
    /// Only valid for purge. Includes every trash batch in this system.
    #[serde(default)]
    pub empty_trash: bool,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryImpact {
    pub version: String,
    pub folder_count: i64,
    pub wordlist_count: i64,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LibrarySelectionInput {
    pub expected_version: String,
    pub node_ids: Vec<Uuid>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LibrarySelection {
    pub version: String,
    pub wordlists: Vec<Wordlist>,
}
