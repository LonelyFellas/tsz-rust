use super::*;
use crate::request_id::RequestId;
use axum::Extension;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceVisibilityInput {
    pub base_revision: i64,
    pub sense_id: Uuid,
    pub hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceVisibilityResponse {
    pub entry_id: Uuid,
    pub revision: i64,
    pub sense_id: Uuid,
    pub sentence_id: Uuid,
    pub hidden: bool,
}

#[utoipa::path(put,path="/api/v1/admin/lexicon/entries/{entry_id}/sentences/{sentence_id}/visibility",tag="admin-lexicon",security(("bearer_auth"=[])),params(("entry_id"=Uuid,Path),("sentence_id"=Uuid,Path)),request_body=SentenceVisibilityInput,responses((status=200,body=SentenceVisibilityResponse),(status=403,description="无权编辑宿主"),(status=409,description="宿主版本或引用冲突")))]
pub async fn set_visibility(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request): Extension<RequestId>,
    ApiPath((entry_id, sentence_id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(input): ApiJson<SentenceVisibilityInput>,
) -> Result<Json<SentenceVisibilityResponse>, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    crate::lexicon::repository::LexiconRepository::lock_surface_contexts(&mut tx, &[entry_id])
        .await
        .map_err(|_| {
            AppError::conflict(ErrorCode::ReferenceConflict, None, "宿主正在修改，请重试")
        })?;
    let row = sqlx::query(
        "SELECT revision,content_schema_version FROM lexicon.entries WHERE id=$1 FOR UPDATE",
    )
    .bind(entry_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("词条不存在"))?;
    writable_entry(&mut tx, entry_id, &admin).await?;
    let revision: i64 = row.get("revision");
    if revision != input.base_revision {
        return Err(conflict());
    }
    if row.get::<i16, _>("content_schema_version") != 3 {
        return Err(invalid("仅支持 V3 词条"));
    }
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon.nodes WHERE id=$1 AND entry_id=$2 AND node_type='sense')")
        .bind(input.sense_id).bind(entry_id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    if !exists {
        return Err(invalid("词义不属于宿主词条"));
    }
    let found=sqlx::query_scalar::<_,Uuid>("SELECT id FROM lexicon.shared_sentences WHERE id=$1 AND deleted_at IS NULL FOR SHARE NOWAIT")
        .bind(sentence_id).fetch_optional(&mut *tx).await.map_err(publication::reference_error)?;
    if found.is_none() {
        return Err(missing());
    }
    if input.hidden {
        let associated:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon.shared_sentence_annotations WHERE sentence_id=$1 AND target_entry_id=$2 AND target_sense_id=$3 UNION ALL SELECT 1 FROM lexicon.shared_sentence_publication_annotations a JOIN lexicon.shared_sentences s ON s.current_publication_id=a.publication_id WHERE s.id=$1 AND a.target_entry_id=$2 AND a.target_sense_id=$3)")
            .bind(sentence_id).bind(entry_id).bind(input.sense_id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
        if !associated {
            return Err(invalid("例句未关联该词义"));
        }
    }
    let changed=if input.hidden {
        sqlx::query("INSERT INTO lexicon.shared_sentence_draft_hides(entry_id,sense_id,sentence_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(entry_id).bind(input.sense_id).bind(sentence_id).execute(&mut *tx).await
    }else{
        sqlx::query("DELETE FROM lexicon.shared_sentence_draft_hides WHERE entry_id=$1 AND sense_id=$2 AND sentence_id=$3")
            .bind(entry_id).bind(input.sense_id).bind(sentence_id).execute(&mut *tx).await
    }.map_err(AppError::internal)?.rows_affected()>0;
    let next = revision + i64::from(changed);
    if changed {
        sqlx::query("UPDATE lexicon.entries SET revision=$2,updated_by_admin_id=$3,updated_at=now() WHERE id=$1")
            .bind(entry_id).bind(next).bind(admin.id).execute(&mut *tx).await.map_err(AppError::internal)?;
        for query in [
            "UPDATE lexicon.entry_editor_projection SET rebuilt_revision=$2,updated_at=now() WHERE entry_id=$1",
            "UPDATE lexicon.entry_presentation_projection SET source_revision=$2,updated_at=now() WHERE entry_id=$1",
            "UPDATE lexicon.surface_sources SET source_revision=$2,updated_at=now() WHERE entry_id=$1 AND content_scope='draft' AND NOT is_deleted",
        ] {
            sqlx::query(query)
                .bind(entry_id)
                .bind(next)
                .execute(&mut *tx)
                .await
                .map_err(AppError::internal)?;
        }
        sqlx::query("INSERT INTO audit.admin_actions(id,actor_admin_id,action,resource_type,resource_id,resource_revision,request_id,metadata) VALUES($1,$2,'lexicon.entry.shared_sentence_visibility_saved','lexicon.entry',$3,$4,$5,$6)")
            .bind(Uuid::now_v7()).bind(admin.id).bind(entry_id).bind(next).bind(request.as_uuid())
            .bind(serde_json::json!({"sentence_id":sentence_id,"sense_id":input.sense_id,"hidden":input.hidden})).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("INSERT INTO platform.outbox_events(id,aggregate_type,aggregate_id,aggregate_revision,event_type,payload,occurred_at,available_at) VALUES($1,'lexicon.entry',$2,$3,'lexicon.entry.draft_meanings_saved',$4,now(),now())")
            .bind(Uuid::now_v7()).bind(entry_id).bind(next).bind(serde_json::json!({"entry_id":entry_id,"source_revision":next,"content_schema_version":3})).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(SentenceVisibilityResponse {
        entry_id,
        revision: next,
        sense_id: input.sense_id,
        sentence_id,
        hidden: input.hidden,
    }))
}
