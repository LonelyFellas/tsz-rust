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

pub(crate) fn page_meta(page: u32, size: u32, total: i64) -> PaginationMeta {
    PaginationMeta {
        page,
        page_size: size,
        total,
        total_pages: (total + i64::from(size) - 1) / i64::from(size),
    }
}

#[cfg(test)]
mod tests {
    use super::page_meta;

    #[test]
    fn boundary_pagination_preserves_empty_exact_and_partial_pages() {
        for (page, size, total, pages) in [(1, 50, 0, 0), (2, 50, 100, 2), (3, 50, 101, 3)] {
            assert_eq!(
                serde_json::to_value(page_meta(page, size, total)).unwrap(),
                serde_json::json!({"page":page,"page_size":size,"total":total,"total_pages":pages})
            );
        }
    }
}
