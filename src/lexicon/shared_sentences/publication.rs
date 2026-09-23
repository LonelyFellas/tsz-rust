use axum::{Extension, Json, extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use utoipa::ToSchema;

use super::*;
use crate::{
    lexicon::{
        dto::AdminWordV3,
        handler::{idempotency_key_error, required_idempotency_key},
        service::v3_publication::PublicationBatchContext,
    },
    request_id::RequestId,
};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentencePublicationInput {
    pub base_revision: i64,
    pub base_lifecycle_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceBatchItem {
    pub sentence_id: Uuid,
    pub base_revision: i64,
    pub base_lifecycle_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentencePublication {
    pub id: Uuid,
    pub sentence_id: Uuid,
    pub publication_number: i64,
    pub source_revision: i64,
    pub snapshot: SharedSentenceContent,
    pub published_at: DateTime<Utc>,
    pub published_by_admin_id: Uuid,
    pub rollback_of_publication_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceImpactTarget {
    pub entry_id: Uuid,
    pub sense_id: Uuid,
    pub lifecycle_revision: i64,
    pub hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceWithdrawalImpact {
    pub sentence_id: Uuid,
    pub lifecycle_revision: i64,
    pub publication_id: Uuid,
    pub targets: Vec<SentenceImpactTarget>,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WithdrawSentenceInput {
    pub base_revision: i64,
    pub base_lifecycle_revision: i64,
    pub reason: String,
    pub impact_fingerprint: String,
}

pub(crate) struct PreparedSentence {
    pub id: Uuid,
    pub publication_id: Uuid,
    pub content: SharedSentenceContent,
    pub revision: i64,
    pub lifecycle_revision: i64,
    pub rollback_of: Option<Uuid>,
    pub is_new: bool,
}

pub(crate) fn digest(value: &impl Serialize) -> Result<String, AppError> {
    Ok(hash_bytes(value)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn hash_bytes(value: &impl Serialize) -> Result<Vec<u8>, AppError> {
    Ok(Sha256::digest(serde_json::to_vec(value).map_err(AppError::internal)?).to_vec())
}

pub(crate) async fn publisher(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
) -> Result<bool, AppError> {
    crate::admin::publication_permission::lock_publisher(tx, actor)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::forbidden(ErrorCode::Forbidden, "需要词库发布权限"))
}

pub(crate) async fn owner(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    actor: Uuid,
    super_admin: bool,
    input: &SentencePublicationInput,
) -> Result<(), AppError> {
    let row = sqlx::query("SELECT revision,lifecycle_revision,created_by_admin_id FROM lexicon.shared_sentences WHERE id=$1 AND deleted_at IS NULL FOR UPDATE")
        .bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?.ok_or_else(missing)?;
    if !super_admin && row.get::<Uuid, _>("created_by_admin_id") != actor {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "仅创建者或超管可以改变例句发布状态",
        ));
    }
    if row.get::<i64, _>("revision") != input.base_revision
        || row.get::<i64, _>("lifecycle_revision") != input.base_lifecycle_revision
    {
        return Err(conflict());
    }
    Ok(())
}

pub(crate) async fn replay(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    actor: Uuid,
    key: Uuid,
    hash: &[u8],
) -> Result<Option<SharedSentence>, AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("{scope}:{actor}:{key}"))
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("DELETE FROM platform.idempotency_records WHERE scope=$1 AND actor_id=$2 AND idempotency_key=$3 AND expires_at<=now()")
        .bind(scope).bind(actor).bind(key).execute(&mut **tx).await.map_err(AppError::internal)?;
    let row = sqlx::query("SELECT request_hash,response_body FROM platform.idempotency_records WHERE scope=$1 AND actor_id=$2 AND idempotency_key=$3 FOR UPDATE")
        .bind(scope).bind(actor).bind(key).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
    row.map(|row| {
        if row.get::<Vec<u8>, _>("request_hash").as_slice() != hash {
            return Err(AppError::conflict(
                ErrorCode::IdempotencyConflict,
                None,
                "幂等键已用于其他请求",
            ));
        }
        serde_json::from_value(row.get("response_body")).map_err(AppError::internal)
    })
    .transpose()
}

async fn remember(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    actor: Uuid,
    key: Uuid,
    hash: &[u8],
    response: &SharedSentence,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO platform.idempotency_records(scope,idempotency_key,actor_id,request_hash,resource_id,response_status,response_body,expires_at) VALUES($1,$2,$3,$4,$5,200,$6,now()+interval '24 hours')")
        .bind(scope).bind(key).bind(actor).bind(hash).bind(response.id)
        .bind(serde_json::to_value(response).map_err(AppError::internal)?)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}

pub(crate) async fn record_event(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    request: Uuid,
    id: Uuid,
    revision: i64,
    action: &str,
    metadata: serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO audit.admin_actions(id,actor_admin_id,action,resource_type,resource_id,resource_revision,request_id,metadata) VALUES($1,$2,$3,'lexicon.shared_sentence',$4,$5,$6,$7)")
        .bind(Uuid::now_v7()).bind(actor).bind(action).bind(id).bind(revision).bind(request).bind(&metadata)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    sqlx::query("INSERT INTO platform.outbox_events(id,aggregate_type,aggregate_id,aggregate_revision,event_type,payload,occurred_at,available_at) VALUES($1,'lexicon.shared_sentence',$2,$3,$4,$5,now(),now())")
        .bind(Uuid::now_v7()).bind(id).bind(revision).bind(action).bind(metadata)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}

pub(crate) async fn validate_published_targets(
    tx: &mut Transaction<'_, Postgres>,
    content: &mut SharedSentenceContent,
    batch: Option<&PublicationBatchContext>,
) -> Result<(), AppError> {
    validate(content)?;
    for annotation in &mut content.annotations {
        let Some(link) = annotation
            .target
            .as_text_link(annotation.id, annotation.source_segments.clone())
        else {
            if matches!(annotation.target, SentenceTarget::EntryOnly { .. }) {
                return Err(invalid("发布前必须补全旧词义关联"));
            }
            continue;
        };
        let (word, publication_id) = if let Some(candidate) =
            batch.and_then(|b| b.words.get(&link.target_word_id))
        {
            (candidate.word.clone(), candidate.publication_id)
        } else {
            let row = sqlx::query("SELECT p.id,p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=$1 AND e.archived_at IS NULL FOR SHARE OF e NOWAIT")
                .bind(link.target_word_id).fetch_optional(&mut **tx).await.map_err(reference_error)?
                .ok_or_else(|| invalid("关联目标尚未发布或已归档，请显式选择依赖一起发布"))?;
            (
                serde_json::from_value::<AdminWordV3>(row.get("snapshot"))
                    .map_err(AppError::internal)?,
                row.get("id"),
            )
        };
        if !crate::lexicon::service::text_links::shared_target_matches(
            &word.forms,
            &word.meanings,
            &link,
            &annotation.source_dialect,
        ) {
            return Err(invalid("已发布目标词义、词形或词面不匹配"));
        }
        if let SentenceTarget::Linked {
            target_publication_id,
            ..
        } = &mut annotation.target
        {
            *target_publication_id = Some(publication_id);
        }
    }
    Ok(())
}

pub(crate) fn reference_error(error: sqlx::Error) -> AppError {
    if error
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|c| c == "55P03" || c == "40P01")
    {
        AppError::conflict(ErrorCode::ReferenceConflict, None, "引用正在被修改，请重试")
    } else {
        AppError::internal(error)
    }
}

pub(crate) async fn prepare(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    super_admin: bool,
    item: &SentenceBatchItem,
    historical: Option<(Uuid, SharedSentenceContent, i64)>,
    batch: Option<&PublicationBatchContext>,
) -> Result<PreparedSentence, AppError> {
    owner(
        tx,
        item.sentence_id,
        actor,
        super_admin,
        &SentencePublicationInput {
            base_revision: item.base_revision,
            base_lifecycle_revision: item.base_lifecycle_revision,
        },
    )
    .await?;
    let (mut content, rollback_of, source_revision) = match historical {
        Some((id, content, source_revision)) => (content, Some(id), source_revision),
        None => (
            read_on(tx, item.sentence_id).await?.content,
            None,
            item.base_revision,
        ),
    };
    let existing = if rollback_of.is_none() {
        sqlx::query("SELECT id,snapshot FROM lexicon.shared_sentence_publications WHERE sentence_id=$1 AND source_revision=$2 AND rollback_of_publication_id IS NULL")
            .bind(item.sentence_id).bind(source_revision).fetch_optional(&mut **tx).await.map_err(AppError::internal)?
    } else {
        None
    };
    if let Some(row) = &existing {
        content = serde_json::from_value(row.get("snapshot")).map_err(AppError::internal)?;
    }
    validate_published_targets(tx, &mut content, batch).await?;
    // Reusing a publication must not rewrite its immutable annotation anchors.
    if let Some(row) = &existing {
        content = serde_json::from_value(row.get("snapshot")).map_err(AppError::internal)?;
    }
    Ok(PreparedSentence {
        id: item.sentence_id,
        publication_id: existing
            .as_ref()
            .map_or_else(Uuid::now_v7, |row| row.get("id")),
        content,
        revision: source_revision,
        lifecycle_revision: item.base_lifecycle_revision,
        rollback_of,
        is_new: existing.is_none(),
    })
}

pub(crate) async fn insert_version(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    prepared: &PreparedSentence,
) -> Result<(), AppError> {
    if prepared.is_new {
        sqlx::query("INSERT INTO lexicon.shared_sentence_publications(id,sentence_id,publication_number,source_revision,snapshot,published_by_admin_id,rollback_of_publication_id) SELECT $1,$2,COALESCE(max(publication_number),0)+1,$3,$4,$5,$6 FROM lexicon.shared_sentence_publications WHERE sentence_id=$2")
            .bind(prepared.publication_id).bind(prepared.id).bind(prepared.revision)
            .bind(serde_json::to_value(&prepared.content).map_err(AppError::internal)?).bind(actor).bind(prepared.rollback_of)
            .execute(&mut **tx).await.map_err(AppError::internal)?;
    }
    Ok(())
}

pub(crate) async fn activate(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    request: Uuid,
    prepared: &PreparedSentence,
) -> Result<SharedSentence, AppError> {
    if prepared.is_new {
        for a in &prepared.content.annotations {
            let (entry, sense, reference, kind, headword, normalized, gloss) = match &a.target {
                SentenceTarget::Linked {
                    target_entry_id,
                    target_sense_id,
                    ..
                } => (
                    Some(*target_entry_id),
                    Some(*target_sense_id),
                    Some(serde_json::to_value(&a.target).map_err(AppError::internal)?),
                    None,
                    None,
                    None,
                    None,
                ),
                SentenceTarget::Pending {
                    kind,
                    headword,
                    gloss,
                } => (
                    None,
                    None,
                    None,
                    Some(kind),
                    Some(headword),
                    Some(
                        normalize_headword(headword)
                            .map_err(|_| invalid("词面无效"))?
                            .key,
                    ),
                    gloss.as_ref(),
                ),
                SentenceTarget::EntryOnly { .. } => return Err(invalid("关联必须精确到词义")),
            };
            sqlx::query("INSERT INTO lexicon.shared_sentence_publication_annotations(publication_id,sentence_id,id,source_dialect,source_segments,target_entry_id,target_sense_id,target_ref,pending_kind,pending_headword,pending_normalized,pending_gloss) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
                .bind(prepared.publication_id).bind(prepared.id).bind(a.id).bind(&a.source_dialect)
                .bind(serde_json::to_value(&a.source_segments).map_err(AppError::internal)?)
                .bind(entry).bind(sense).bind(reference).bind(kind).bind(headword).bind(normalized).bind(gloss)
                .execute(&mut **tx).await.map_err(AppError::internal)?;
        }
    }
    let changed = sqlx::query("UPDATE lexicon.shared_sentences SET current_publication_id=$2,lifecycle_revision=lifecycle_revision+1 WHERE id=$1 AND lifecycle_revision=$3 AND current_publication_id IS DISTINCT FROM $2")
        .bind(prepared.id).bind(prepared.publication_id).bind(prepared.lifecycle_revision)
        .execute(&mut **tx).await.map_err(AppError::internal)?.rows_affected();
    if changed == 1 {
        record_event(tx,actor,request,prepared.id,prepared.lifecycle_revision+1,
            if prepared.rollback_of.is_some() {"lexicon.shared_sentence.rolled_back"} else {"lexicon.shared_sentence.published"},
            serde_json::json!({"publication_id":prepared.publication_id,"rollback_of_publication_id":prepared.rollback_of})).await?;
    }
    read_on(tx, prepared.id).await
}

async fn publish_command(
    state: &AppState,
    actor: Uuid,
    request: Uuid,
    id: Uuid,
    key: Uuid,
    input: SentencePublicationInput,
    historical_id: Option<Uuid>,
) -> Result<SharedSentence, AppError> {
    let scope = if historical_id.is_some() {
        "lexicon.shared_sentence.rollback"
    } else {
        "lexicon.shared_sentence.publish"
    };
    let hash =
        hash_bytes(&serde_json::json!({"id":id,"input":input,"historical_id":historical_id}))?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let super_admin = publisher(&mut tx, actor).await?;
    if let Some(response) = replay(&mut tx, scope, actor, key, &hash).await? {
        tx.commit().await.map_err(AppError::internal)?;
        return Ok(response);
    }
    let historical = if let Some(publication_id) = historical_id {
        let row = sqlx::query("SELECT snapshot,source_revision FROM lexicon.shared_sentence_publications WHERE id=$1 AND sentence_id=$2")
            .bind(publication_id).bind(id).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.ok_or_else(missing)?;
        Some((
            publication_id,
            serde_json::from_value::<SharedSentenceContent>(row.get("snapshot"))
                .map_err(AppError::internal)?,
            row.get("source_revision"),
        ))
    } else {
        None
    };
    let content = match &historical {
        Some((_, content, _)) => content.clone(),
        None => read_on(&mut tx, id).await?.content,
    };
    lock_targets(&mut tx, Some(&content), None, Some(id)).await?;
    let prepared = prepare(
        &mut tx,
        actor,
        super_admin,
        &SentenceBatchItem {
            sentence_id: id,
            base_revision: input.base_revision,
            base_lifecycle_revision: input.base_lifecycle_revision,
        },
        historical,
        None,
    )
    .await?;
    insert_version(&mut tx, actor, &prepared).await?;
    let response = activate(&mut tx, actor, request, &prepared).await?;
    remember(&mut tx, scope, actor, key, &hash, &response).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(response)
}

#[utoipa::path(post,path="/api/v1/admin/lexicon/sentences/{id}/publications",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("Idempotency-Key"=Uuid,Header)),request_body=SentencePublicationInput,responses((status=200,body=SharedSentence),(status=403,description="无发布权或非创建者"),(status=409,description="版本或引用冲突")))]
pub async fn publish(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<SentencePublicationInput>,
) -> Result<Json<SharedSentence>, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    Ok(Json(
        publish_command(&state, admin.id, request.as_uuid(), id, key, input, None).await?,
    ))
}

#[utoipa::path(post,path="/api/v1/admin/lexicon/sentences/{id}/publications/{publication_id}/rollback",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("publication_id"=Uuid,Path),("Idempotency-Key"=Uuid,Header)),request_body=SentencePublicationInput,responses((status=200,body=SharedSentence),(status=403,description="无发布权或非创建者"),(status=409,description="版本或引用冲突")))]
pub async fn rollback(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath((id, publication)): ApiPath<(Uuid, Uuid)>,
    ApiJson(input): ApiJson<SentencePublicationInput>,
) -> Result<Json<SharedSentence>, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    Ok(Json(
        publish_command(
            &state,
            admin.id,
            request.as_uuid(),
            id,
            key,
            input,
            Some(publication),
        )
        .await?,
    ))
}

async fn impact_on(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<SentenceWithdrawalImpact, AppError> {
    let (publication_id,lifecycle_revision): (Option<Uuid>,i64)=sqlx::query_as("SELECT current_publication_id,lifecycle_revision FROM lexicon.shared_sentences WHERE id=$1 AND deleted_at IS NULL")
        .bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?.ok_or_else(missing)?;
    let publication_id = publication_id.ok_or_else(|| invalid("例句尚未发布"))?;
    let rows=sqlx::query("SELECT DISTINCT a.target_entry_id,a.target_sense_id,e.lifecycle_revision,EXISTS(SELECT 1 FROM lexicon.shared_sentence_publication_hides h WHERE h.publication_id=e.current_publication_id AND h.sense_id=a.target_sense_id AND h.sentence_id=a.sentence_id) AS hidden FROM lexicon.shared_sentence_publication_annotations a JOIN lexicon.entries e ON e.id=a.target_entry_id WHERE a.publication_id=$1 ORDER BY a.target_entry_id,a.target_sense_id")
        .bind(publication_id).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let targets = rows
        .into_iter()
        .map(|r| SentenceImpactTarget {
            entry_id: r.get("target_entry_id"),
            sense_id: r.get("target_sense_id"),
            lifecycle_revision: r.get("lifecycle_revision"),
            hidden: r.get("hidden"),
        })
        .collect::<Vec<_>>();
    let fingerprint = digest(&(id, publication_id, lifecycle_revision, &targets))?;
    Ok(SentenceWithdrawalImpact {
        sentence_id: id,
        publication_id,
        lifecycle_revision,
        targets,
        fingerprint,
    })
}

async fn current_content(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<SharedSentenceContent, AppError> {
    let value:serde_json::Value=sqlx::query_scalar("SELECT p.snapshot FROM lexicon.shared_sentences s JOIN lexicon.shared_sentence_publications p ON p.id=s.current_publication_id WHERE s.id=$1 AND s.deleted_at IS NULL")
        .bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?.ok_or_else(missing)?;
    serde_json::from_value(value).map_err(AppError::internal)
}

#[utoipa::path(get,path="/api/v1/admin/lexicon/sentences/{id}/withdrawal-impact",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,body=SentenceWithdrawalImpact),(status=404,description="例句未发布")))]
pub async fn impact(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<SentenceWithdrawalImpact>, AppError> {
    require_active_admin(&state, &auth).await?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    Ok(Json(impact_on(&mut tx, id).await?))
}

#[allow(clippy::too_many_arguments)]
async fn visibility_command(
    state: &AppState,
    actor: Uuid,
    request: Uuid,
    id: Uuid,
    key: Uuid,
    input: SentencePublicationInput,
    withdraw: Option<(String, String)>,
) -> Result<SharedSentence, AppError> {
    let scope = if withdraw.is_some() {
        "lexicon.shared_sentence.withdraw"
    } else {
        "lexicon.shared_sentence.restore"
    };
    if withdraw
        .as_ref()
        .is_some_and(|(reason, _)| reason.trim().is_empty() || reason.chars().count() > 2000)
    {
        return Err(invalid("下架原因须为 1–2000 个字符"));
    }
    let hash = hash_bytes(&serde_json::json!({"id":id,"input":input,"withdraw":withdraw}))?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let super_admin = publisher(&mut tx, actor).await?;
    if let Some(response) = replay(&mut tx, scope, actor, key, &hash).await? {
        tx.commit().await.map_err(AppError::internal)?;
        return Ok(response);
    }
    let mut content = current_content(&mut tx, id).await?;
    lock_targets(&mut tx, None, None, Some(id)).await?;
    owner(&mut tx, id, actor, super_admin, &input).await?;
    // The current publication may have changed before the sentence row was acquired.
    let locked_content = current_content(&mut tx, id).await?;
    if digest(&content)? != digest(&locked_content)? {
        return Err(conflict());
    }
    if let Some((reason, fingerprint)) = &withdraw {
        if impact_on(&mut tx, id).await?.fingerprint != *fingerprint {
            return Err(conflict());
        }
        sqlx::query("UPDATE lexicon.shared_sentences SET withdrawn_at=now(),withdrawn_reason=$2,lifecycle_revision=lifecycle_revision+1 WHERE id=$1")
            .bind(id).bind(reason.trim()).execute(&mut *tx).await.map_err(AppError::internal)?;
    } else {
        validate_published_targets(&mut tx, &mut content, None).await?;
        sqlx::query("UPDATE lexicon.shared_sentences SET withdrawn_at=NULL,withdrawn_reason=NULL,lifecycle_revision=lifecycle_revision+1 WHERE id=$1")
            .bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    record_event(
        &mut tx,
        actor,
        request,
        id,
        input.base_lifecycle_revision + 1,
        scope,
        serde_json::json!({"withdrawal":withdraw}),
    )
    .await?;
    let response = read_on(&mut tx, id).await?;
    remember(&mut tx, scope, actor, key, &hash, &response).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(response)
}

#[utoipa::path(post,path="/api/v1/admin/lexicon/sentences/{id}/withdraw",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("Idempotency-Key"=Uuid,Header)),request_body=WithdrawSentenceInput,responses((status=200,body=SharedSentence),(status=403,description="无下架权"),(status=409,description="版本或影响确认过期")))]
pub async fn withdraw(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<WithdrawSentenceInput>,
) -> Result<Json<SharedSentence>, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    Ok(Json(
        visibility_command(
            &state,
            admin.id,
            request.as_uuid(),
            id,
            key,
            SentencePublicationInput {
                base_revision: input.base_revision,
                base_lifecycle_revision: input.base_lifecycle_revision,
            },
            Some((input.reason, input.impact_fingerprint)),
        )
        .await?,
    ))
}

#[utoipa::path(post,path="/api/v1/admin/lexicon/sentences/{id}/restore",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("Idempotency-Key"=Uuid,Header)),request_body=SentencePublicationInput,responses((status=200,body=SharedSentence),(status=403,description="无恢复权"),(status=409,description="版本或引用冲突")))]
pub async fn restore(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<SentencePublicationInput>,
) -> Result<Json<SharedSentence>, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    Ok(Json(
        visibility_command(&state, admin.id, request.as_uuid(), id, key, input, None).await?,
    ))
}
