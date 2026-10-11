//! Independently authorized user wallet, ledger and identity-document views.
use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiPath, ApiQuery, PaginationMeta},
    coins::{
        dto::{CoinEntriesQuery, CoinWallet},
        model::{OperationKind, Owner, OwnerType},
        repository,
    },
    error::AppError,
    state::AppState,
    teacher_certification::files::{self, CertificationFile, CertificationImage},
};
use axum::{Json, extract::State, response::Response};
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

async fn authorize(
    state: &AppState,
    auth: &AdminAuth,
    id: Uuid,
    permission: &str,
) -> Result<(), AppError> {
    permissions::load(state, auth).await?.require(permission)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1)")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::internal)?;
    if !exists {
        return Err(AppError::not_found("用户不存在"));
    }
    Ok(())
}

#[utoipa::path(get, path="/api/v1/admin/users/{id}/coin-wallet", tag="admin-users", security(("bearer_auth"=[])),
params(("id"=Uuid,Path)), responses((status=200,body=CoinWallet),(status=403),(status=404)))]
pub async fn wallet(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<CoinWallet>, AppError> {
    authorize(&state, &auth, id, "users.read_coin_balance").await?;
    let response = repository::wallet(
        &state.pool,
        Owner {
            owner_type: OwnerType::User,
            owner_id: id,
        },
    )
    .await
    .map_err(AppError::internal)?;
    permissions::reload(&state, &auth)
        .await?
        .require("users.read_coin_balance")?;
    Ok(Json(response))
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserCoinEntry {
    #[schema(pattern = "^[1-9][0-9]{0,18}$")]
    pub id: String,
    pub operation_id: Uuid,
    pub kind: OperationKind,
    pub source_type: String,
    #[schema(pattern = "^-?[1-9][0-9]{0,18}$")]
    pub delta: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub balance_after: Option<String>,
    pub created_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserCoinEntryPage {
    pub items: Vec<UserCoinEntry>,
    pub pagination: PaginationMeta,
    #[schema(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub snapshot: String,
}
#[utoipa::path(get, path="/api/v1/admin/users/{id}/coin-entries", tag="admin-users", security(("bearer_auth"=[])),
params(("id"=Uuid,Path),CoinEntriesQuery), responses((status=200,body=UserCoinEntryPage),(status=400),(status=403),(status=404)))]
pub async fn entries(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<CoinEntriesQuery>,
) -> Result<Json<UserCoinEntryPage>, AppError> {
    authorize(&state, &auth, id, "users.read_coin_entries").await?;
    let response = repository::entries(
        &state.pool,
        Owner {
            owner_type: OwnerType::User,
            owner_id: id,
        },
        query,
    )
    .await?;
    let authorization = permissions::reload(&state, &auth).await?;
    authorization.require("users.read_coin_entries")?;
    let can_balance = authorization.has("users.read_coin_balance");
    Ok(Json(UserCoinEntryPage {
        items: response
            .items
            .into_iter()
            .map(|e| UserCoinEntry {
                id: e.id,
                operation_id: e.operation_id,
                kind: e.kind,
                source_type: e.source_type,
                delta: e.delta,
                balance_after: can_balance.then_some(e.balance_after),
                created_at: e.created_at,
            })
            .collect(),
        pagination: response.pagination,
        snapshot: response.snapshot,
    }))
}

#[utoipa::path(get, path="/api/v1/admin/users/{id}/identity-documents", tag="admin-users", security(("bearer_auth"=[])),
params(("id"=Uuid,Path)), responses((status=200,body=Vec<CertificationFile>),(status=403),(status=404)))]
pub async fn identity_documents(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<Vec<CertificationFile>>, AppError> {
    authorize(&state, &auth, id, "users.read_identity").await?;
    let files = sqlx::query_as::<_, CertificationFile>("SELECT f.* FROM teacher_certification_files f JOIN teacher_application_files a ON a.file_id=f.id WHERE f.user_id=$1 AND f.kind IN ('id_front','id_back') AND f.state='ready' AND a.application_id=(SELECT id FROM teacher_applications WHERE user_id=$1 ORDER BY submitted_at DESC,id DESC LIMIT 1) ORDER BY a.position")
        .bind(id).fetch_all(&state.pool).await.map_err(AppError::internal)?;
    permissions::reload(&state, &auth)
        .await?
        .require("users.read_identity")?;
    Ok(Json(files))
}

#[utoipa::path(get, path="/api/v1/admin/users/{id}/identity-documents/{file_id}", tag="admin-users", security(("bearer_auth"=[])),
params(("id"=Uuid,Path),("file_id"=Uuid,Path)), responses((status=200,content((inline(CertificationImage)="image/jpeg"),(inline(CertificationImage)="image/png"),(inline(CertificationImage)="image/webp"))),(status=403),(status=404)))]
pub async fn identity_file(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath((id, file_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<Response, AppError> {
    let Json(documents) =
        identity_documents(State(state.clone()), auth.clone(), ApiPath(id)).await?;
    if !documents.iter().any(|file| file.id == file_id) {
        return Err(AppError::not_found("身份证件不存在"));
    }
    let response = files::read(&state, file_id, Some(id)).await?;
    permissions::reload(&state, &auth)
        .await?
        .require("users.read_identity")?;
    Ok(response)
}
