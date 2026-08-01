use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginationMeta {
    pub page: u32,
    pub page_size: u32,
    pub total: i64,
    pub total_pages: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedResponse<T> {
    pub items: Vec<T>,
    pub pagination: PaginationMeta,
}
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PaginationQuery {
    /// 每页，从1开始
    #[param(default = 1, minimum = 1)]
    pub page: Option<u32>,

    /// 每页数量, 范围1-100
    #[param(default = 20, minimum = 1, maximum = 100)]
    pub page_size: Option<u32>,
}

#[derive(Debug)]
pub struct ListQuery<T> {
    pub filters: T,

    pub pagination: PaginationQuery,
}
