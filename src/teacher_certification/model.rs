use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct TeacherApplication {
    pub id: Uuid,
    pub user_id: Uuid,
    pub real_name: String,
    pub contact: String,
    pub statement: String,
    pub status: String,
    pub submitted_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub review_reason: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct TeacherCertification {
    pub teacher_verified: bool,
    pub application: Option<TeacherApplication>,
    pub files: Vec<super::files::CertificationFile>,
}

#[derive(Serialize, ToSchema)]
pub struct TeacherApplicationDetail {
    pub application: TeacherApplication,
    pub files: Vec<super::files::CertificationFile>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub status: Option<String>,
}

impl ListQuery {
    pub fn pagination(&self) -> Result<(i64, i64), crate::error::AppError> {
        let page = self.page.unwrap_or(1);
        let size = self.page_size.unwrap_or(20);
        if page == 0 || size == 0 || size > 100 {
            return Err(super::service::invalid("分页参数无效"));
        }
        Ok((i64::from(size), i64::from(page - 1) * i64::from(size)))
    }
}

#[derive(Serialize, ToSchema)]
pub struct TeacherApplicationList {
    pub items: Vec<TeacherApplication>,
    pub total: i64,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitApplication {
    pub real_name: String,
    pub contact: String,
    pub statement: String,
    pub id_front: Uuid,
    pub id_back: Uuid,
    pub education_files: Vec<Uuid>,
    pub language_files: Vec<Uuid>,
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approve,
    Reject,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewApplication {
    pub decision: ReviewDecision,
    pub reason: Option<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeCertification {
    pub reason: String,
}

#[derive(Serialize, FromRow, ToSchema)]
pub struct UserNotification {
    pub id: Uuid,
    pub application_id: Option<Uuid>,
    pub kind: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub struct NotificationList {
    pub items: Vec<UserNotification>,
    pub total: i64,
    pub unread_count: i64,
}
