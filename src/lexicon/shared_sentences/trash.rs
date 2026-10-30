//! Recoverable deletion is separate from publication withdrawal and legacy permanent deletion.
use super::*;

async fn change(
    state: AppState,
    auth: AdminAuth,
    id: Uuid,
    input: SentenceRevision,
    restoring: bool,
) -> Result<StatusCode, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let authorization = crate::admin::permissions::lock(&mut tx, admin.id).await?;
    if !authorization.is_super_admin {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "仅超管可以管理例句垃圾桶",
        ));
    }
    let mut sentence = read_on_with_trash(&mut tx, id, restoring).await?;
    lock_targets(
        &mut tx,
        if restoring {
            Some(&sentence.content)
        } else {
            None
        },
        None,
        Some(id),
    )
    .await?;
    let row = sqlx::query("SELECT revision,current_publication_id FROM lexicon.shared_sentences WHERE id=$1 AND (($2 AND in_trash) OR (NOT $2 AND deleted_at IS NULL)) FOR UPDATE")
        .bind(id).bind(restoring).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.ok_or_else(missing)?;
    if row.get::<i64, _>("revision") != input.base_revision
        || sentence.revision != input.base_revision
    {
        return Err(conflict());
    }
    if row
        .get::<Option<Uuid>, _>("current_publication_id")
        .is_some()
    {
        return Err(AppError::conflict(
            ErrorCode::ReferenceConflict,
            None,
            "已有发布历史的例句不能移入垃圾桶，请使用撤回",
        ));
    }
    if restoring {
        validate_targets(&mut tx, &sentence.content, None, None).await?;
        crate::lexicon::service::grammar_form_links::validate_reading_text(
            &mut tx,
            &mut sentence.content.sentence.en_text,
            None,
            None,
            false,
        )
        .await
        .map_err(crate::lexicon::handler::map_error)?;
    }
    sqlx::query("UPDATE lexicon.shared_sentences SET in_trash=NOT $2,deleted_at=CASE WHEN $2 THEN NULL ELSE now() END,revision=revision+1,lifecycle_revision=lifecycle_revision+1,updated_at=now() WHERE id=$1")
        .bind(id).bind(restoring).execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post,path="/api/v1/admin/lexicon/sentences/{id}/trash",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=SentenceRevision,responses((status=204,description="草稿已移入垃圾桶，保留关联及音频"),(status=403,description="仅超管"),(status=404,description="例句不存在"),(status=409,description="版本冲突或已有发布历史")))]
pub async fn archive(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<SentenceRevision>,
) -> Result<StatusCode, AppError> {
    change(state, auth, id, input, false).await
}

#[utoipa::path(post,path="/api/v1/admin/lexicon/sentences/{id}/untrash",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=SentenceRevision,responses((status=204,description="已恢复草稿"),(status=400,description="关联目标已失效"),(status=403,description="仅超管"),(status=404,description="例句不在垃圾桶"),(status=409,description="版本冲突")))]
pub async fn restore(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<SentenceRevision>,
) -> Result<StatusCode, AppError> {
    change(state, auth, id, input, true).await
}
