use axum::{Json, extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Connection, PgConnection, Row, postgres::PgAdvisoryLock};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    admin::{AdminAuth, permissions},
    api::ApiJson,
    error::{AppError, ErrorCode},
    platform::storage::ObjectKey,
    state::AppState,
    test_resources,
};

const MAX_ENTRIES: usize = 100;

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TestPurgeRequest {
    pub request_id: Uuid,
    #[schema(min_items = 1, max_items = 100)]
    pub entry_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TestPurgeResponse {
    pub run: String,
    pub entry_ids: Vec<Uuid>,
    pub publication_ids: Vec<Uuid>,
    pub audio_asset_ids: Vec<Uuid>,
}

#[derive(PartialEq)]
struct AudioAsset {
    id: Uuid,
    object_key: ObjectKey,
}

struct PurgePlan {
    entry_ids: Vec<Uuid>,
    publication_ids: Vec<Uuid>,
    assets: Vec<AudioAsset>,
}

fn conflict(message: &'static str) -> AppError {
    AppError::conflict(ErrorCode::RevisionConflict, None, message)
}

fn canonical_request(input: &TestPurgeRequest) -> Result<(TestPurgeRequest, Value), AppError> {
    if input.request_id.is_nil()
        || input.entry_ids.is_empty()
        || input.entry_ids.len() > MAX_ENTRIES
    {
        return Err(AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "test purge request is invalid",
        ));
    }
    let mut ids = input.entry_ids.clone();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != input.entry_ids.len() {
        return Err(AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "test purge entry ids must be unique",
        ));
    }
    let canonical = TestPurgeRequest {
        request_id: input.request_id,
        entry_ids: ids,
    };
    let value = serde_json::to_value(&canonical).map_err(AppError::internal)?;
    Ok((canonical, value))
}

async fn existing_command(
    conn: &mut PgConnection,
    actor: Uuid,
    run: &str,
    request_id: Uuid,
    request: &Value,
) -> Result<Option<TestPurgeResponse>, AppError> {
    let Some((stored_run, stored_request, stored_result)) = sqlx::query_as::<_, (String, Value, Value)>(
        "SELECT run_id,request,result FROM lexicon_next.test_resource_purge_commands WHERE actor_id=$1 AND request_id=$2",
    )
    .bind(actor)
    .bind(request_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(AppError::internal)? else {
        return Ok(None);
    };
    if stored_run != run || stored_request != *request {
        return Err(AppError::conflict(
            ErrorCode::IdempotencyConflict,
            None,
            "请求编号已用于其他测试清理请求",
        ));
    }
    serde_json::from_value(stored_result)
        .map(Some)
        .map_err(AppError::internal)
}

async fn ensure_no_external_references(
    conn: &mut PgConnection,
    entry_ids: &[Uuid],
) -> Result<(), AppError> {
    let used_by_students: bool = sqlx::query_scalar(
        "SELECT CASE WHEN EXISTS(SELECT FROM learning_source.reviews WHERE entry_id=ANY($1)) THEN
            EXISTS(SELECT FROM wordlist_items WHERE entry_id=ANY($1))
            OR EXISTS(SELECT FROM wordlist_review_requests WHERE entry_ids && $1)
            OR EXISTS(SELECT FROM learning_memories WHERE entry_id=ANY($1))
            OR EXISTS(SELECT FROM learning_questions WHERE entry_id=ANY($1) OR
                jsonb_path_exists(answer_snapshot,'$.choice.content.options[*].candidate.source.entry_id ? (@ == $ids[*])', jsonb_build_object('ids',to_jsonb($1::uuid[]))))
            OR EXISTS(SELECT FROM learning_cycle_units WHERE entry_id=ANY($1) OR
                jsonb_path_exists(answer_snapshot,'$.choice.content.options[*].candidate.source.entry_id ? (@ == $ids[*])', jsonb_build_object('ids',to_jsonb($1::uuid[]))))
            ELSE false END",
    )
    .bind(entry_ids)
    .fetch_one(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if used_by_students {
        return Err(conflict("测试词条仍被学生词表或学习记录引用"));
    }
    let relation_used: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM lexicon_next.entries e
            JOIN lexicon_next.draft_revisions r ON r.entry_id=e.id AND r.revision=e.revision
            CROSS JOIN LATERAL jsonb_array_elements(r.document->'senses') s
            CROSS JOIN LATERAL jsonb_array_elements(COALESCE(s->'relations','[]'::jsonb)) rel
            WHERE e.id <> ALL($1) AND rel->>'target_word_id' = ANY($2)
        )",
    )
    .bind(entry_ids)
    .bind(entry_ids.iter().map(Uuid::to_string).collect::<Vec<_>>())
    .fetch_one(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if relation_used {
        return Err(conflict("测试词条仍被其他词条的拓展词引用"));
    }

    let published_relation_used: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM lexicon_next.entries e
            JOIN LATERAL (
                SELECT snapshot FROM lexicon_next.publications
                WHERE entry_id=e.id ORDER BY lifecycle_revision DESC LIMIT 1
            ) p ON true
            CROSS JOIN LATERAL jsonb_array_elements(COALESCE(p.snapshot->'relation_targets','[]'::jsonb)) rel
            WHERE e.status='published' AND e.id <> ALL($1)
              AND rel->>'entry_id' = ANY($2)
        )",
    )
    .bind(entry_ids)
    .bind(entry_ids.iter().map(Uuid::to_string).collect::<Vec<_>>())
    .fetch_one(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if published_relation_used {
        return Err(conflict("测试词条仍被已发布词条引用"));
    }

    let grammar_used: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM lexicon_next.grammar_form_links WHERE target_word_id=ANY($1) AND NOT (entry_id=ANY($1)))",
    )
    .bind(entry_ids)
    .fetch_one(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    let sentence_used: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM lexicon_next.sentence_associations WHERE target_word_id=ANY($1))",
    )
    .bind(entry_ids)
    .fetch_one(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if grammar_used || sentence_used {
        return Err(conflict("测试词条仍被例句或语法关联引用"));
    }

    let dialect_rows = sqlx::query(
        "SELECT gb_entry,us_entry FROM lexicon_next.dialect_pairs WHERE gb_entry=ANY($1) OR us_entry=ANY($1)",
    )
    .bind(entry_ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if dialect_rows.iter().any(|row| {
        let gb: Uuid = row.get("gb_entry");
        let us: Uuid = row.get("us_entry");
        !entry_ids.contains(&gb) || !entry_ids.contains(&us)
    }) {
        return Err(conflict("测试词条仍有未纳入清理批次的方言配对"));
    }
    Ok(())
}

/// Called under the list-writer lock, which also serializes the first purge transaction.
/// The committed asset marker survives storage failures and blocks lifecycle changes on retry.
pub(super) async fn ensure_lifecycle_available(
    conn: &mut PgConnection,
    entry_id: Uuid,
) -> Result<(), AppError> {
    let purging: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM lexicon_next.test_resource_entries WHERE entry_id=$1)
            AND EXISTS (
                SELECT 1 FROM lexicon.audio_assets a
                WHERE a.reclamation_started_at IS NOT NULL AND a.id IN (
                    SELECT asset_id FROM lexicon_next.audio_asset_references WHERE entry_id=$1
                    UNION ALL
                    SELECT r.asset_id FROM lexicon_next.publications p
                    JOIN lexicon_next.publication_audio_references r ON r.publication_id=p.id
                    WHERE p.entry_id=$1
                )
            )",
    )
    .bind(entry_id)
    .fetch_one(conn)
    .await
    .map_err(AppError::internal)?;
    if purging {
        return Err(conflict("测试词条正在清理音频，请重试原清理请求"));
    }
    Ok(())
}

async fn prepare(
    conn: &mut PgConnection,
    run: &str,
    actor: Uuid,
    entry_ids: &[Uuid],
) -> Result<PurgePlan, AppError> {
    let registered: Vec<Uuid> = sqlx::query_scalar(
        "SELECT entry_id FROM lexicon_next.test_resource_entries WHERE run_id=$1 AND actor_id=$2 AND entry_id=ANY($3) FOR UPDATE",
    )
    .bind(run)
    .bind(actor)
    .bind(entry_ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if registered.len() != entry_ids.len() {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "词条未登记在当前测试批次中",
        ));
    }

    let rows = sqlx::query(
        "SELECT id,owner_id,status FROM lexicon_next.entries WHERE id=ANY($1) FOR UPDATE",
    )
    .bind(entry_ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if rows.len() != entry_ids.len()
        || rows.iter().any(|row| {
            row.get::<Uuid, _>("owner_id") != actor || row.get::<String, _>("status") != "archived"
        })
    {
        return Err(conflict("测试 purge 只允许清理当前批次已归档词条"));
    }
    ensure_no_external_references(conn, entry_ids).await?;

    let publication_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM lexicon_next.publications WHERE entry_id=ANY($1) ORDER BY id",
    )
    .bind(entry_ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    let asset_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT asset_id FROM lexicon_next.audio_asset_references WHERE entry_id=ANY($1)
         UNION
         SELECT DISTINCT reference.asset_id
         FROM lexicon_next.publication_audio_references reference
         JOIN lexicon_next.publications publication ON publication.id=reference.publication_id
         WHERE publication.entry_id=ANY($1)",
    )
    .bind(entry_ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    let asset_rows = sqlx::query(
        "SELECT id,object_key,reclamation_started_at FROM lexicon.audio_assets WHERE id=ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(&asset_ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if asset_rows.len() != asset_ids.len() {
        return Err(conflict("测试词条引用的音频资产不存在"));
    }
    let prefix = format!("temporary-tests/{run}/audio/");
    let mut assets = Vec::with_capacity(asset_rows.len());
    for row in asset_rows {
        let id: Uuid = row.get("id");
        let key: String = row.get("object_key");
        if !key.starts_with(&prefix) {
            return Err(conflict("测试词条包含非当前测试批次的音频对象"));
        }
        assets.push(AudioAsset {
            id,
            object_key: ObjectKey::parse(key).map_err(AppError::internal)?,
        });
    }

    let external_asset_reference: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM lexicon_next.audio_asset_references r
            WHERE r.asset_id=ANY($1) AND NOT (r.entry_id=ANY($2))
            UNION ALL
            SELECT 1 FROM lexicon_next.sentence_audio_asset_references r
            WHERE r.asset_id=ANY($1)
            UNION ALL
            SELECT 1 FROM lexicon_next.publication_audio_references r
            WHERE r.asset_id=ANY($1)
              AND (r.resource_id <> ALL($2)
              AND NOT EXISTS (
                  SELECT 1 FROM lexicon_next.publications p
                  WHERE p.id=r.publication_id AND p.entry_id=ANY($2)
              ))
            UNION ALL
            SELECT 1 FROM lexicon.v3_audio_asset_references r WHERE r.asset_id=ANY($1)
            UNION ALL
            SELECT 1 FROM lexicon.shared_sentence_audio_asset_references r WHERE r.asset_id=ANY($1)
            UNION ALL
            SELECT 1 FROM learning_question_audio r WHERE r.asset_id=ANY($1)
        )",
    )
    .bind(&asset_ids)
    .bind(entry_ids)
    .fetch_one(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if external_asset_reference {
        return Err(conflict("音频资产仍被清理批次之外的数据引用"));
    }

    let pending_audio: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM lexicon.audio_generations WHERE asset_id=ANY($1) AND state='pending')",
    )
    .bind(&asset_ids)
    .fetch_one(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if pending_audio {
        return Err(conflict("音频生成任务仍在进行，暂不能 purge"));
    }
    Ok(PurgePlan {
        entry_ids: entry_ids.to_vec(),
        publication_ids,
        assets,
    })
}

async fn mark_assets(conn: &mut PgConnection, assets: &[AudioAsset]) -> Result<(), AppError> {
    for asset in assets {
        sqlx::query(
            "UPDATE lexicon.audio_assets SET reclamation_started_at=COALESCE(reclamation_started_at,now()) WHERE id=$1",
        )
        .bind(asset.id)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    }
    Ok(())
}

async fn delete_database(conn: &mut PgConnection, plan: &PurgePlan) -> Result<(), AppError> {
    let asset_ids = plan.assets.iter().map(|asset| asset.id).collect::<Vec<_>>();
    sqlx::query(
        "DELETE FROM lexicon_next.publication_audio_references WHERE publication_id=ANY($1)",
    )
    .bind(&plan.publication_ids)
    .execute(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    sqlx::query("DELETE FROM lexicon_next.publication_taxonomy_refs WHERE publication_id=ANY($1)")
        .bind(&plan.publication_ids)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("DELETE FROM lexicon_next.publication_commands WHERE publication_id=ANY($1)")
        .bind(&plan.publication_ids)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("DELETE FROM lexicon_next.publications WHERE id=ANY($1)")
        .bind(&plan.publication_ids)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;

    sqlx::query("DELETE FROM lexicon_next.audio_asset_references WHERE entry_id=ANY($1)")
        .bind(&plan.entry_ids)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("DELETE FROM lexicon.audio_generations WHERE asset_id=ANY($1)")
        .bind(&asset_ids)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("DELETE FROM lexicon.audio_assets WHERE id=ANY($1)")
        .bind(&asset_ids)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;

    sqlx::query(
        "DELETE FROM lexicon_next.dialect_pairs WHERE gb_entry=ANY($1) AND us_entry=ANY($1)",
    )
    .bind(&plan.entry_ids)
    .execute(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    // The command result contains every target ID, including targets from other batches.
    sqlx::query(
        "DELETE FROM lexicon_next.list_commands
         WHERE request->>'action' IN ('archive','restore')
           AND jsonb_typeof(result->'entry_ids')='array'
           AND result->'entry_ids' <> '[]'::jsonb
           AND to_jsonb($1::uuid[]) @> (result->'entry_ids')",
    )
    .bind(&plan.entry_ids)
    .execute(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    for sql in [
        "DELETE FROM lexicon_next.equivalents WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=ANY($1))",
        "DELETE FROM lexicon_next.definitions WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=ANY($1))",
        "DELETE FROM lexicon_next.senses WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=ANY($1))",
        "DELETE FROM lexicon_next.forms WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=ANY($1))",
        "DELETE FROM lexicon_next.nodes WHERE entry_id=ANY($1)",
        "DELETE FROM lexicon_next.draft_revisions WHERE entry_id=ANY($1)",
        "DELETE FROM lexicon_next.entries WHERE id=ANY($1)",
    ] {
        sqlx::query(sql)
            .bind(&plan.entry_ids)
            .execute(&mut *conn)
            .await
            .map_err(AppError::internal)?;
    }
    Ok(())
}

#[utoipa::path(
    post,
    path="/api/v1/admin/lexicon-next/test-resources/purge",
    tag="admin-lexicon-next-test-fixtures",
    security(("bearer_auth"=[])),
    params(("X-TSZ-Test-Run"=String, Header, description="仅测试环境白名单管理员可用的独占批次")),
    request_body=TestPurgeRequest,
    responses((status=200, body=TestPurgeResponse), (status=400), (status=401), (status=403), (status=409), (status=503))
)]
pub async fn purge(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiJson(input): ApiJson<TestPurgeRequest>,
) -> Result<Json<TestPurgeResponse>, AppError> {
    if !state.test_resource_purge_enabled {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "test resource purge is disabled",
        ));
    }
    let (state, run) = test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    let run = run.ok_or_else(|| {
        AppError::forbidden(ErrorCode::Forbidden, "test resource run is required")
    })?;
    let (input, request) = canonical_request(&input)?;
    let authorization = permissions::load(&state, &auth).await?;
    authorization.require("words.access")?;
    authorization.require("words.create")?;
    // Serialize retries across both transactions and storage I/O, without holding
    // the list-writer lock while waiting for storage.
    let command_lock = PgAdvisoryLock::new(format!(
        "lexicon_next.purge/{}/{}",
        auth.subject, input.request_id
    ));
    let connection = state.pool.acquire().await.map_err(AppError::internal)?;
    let mut connection = command_lock
        .acquire(connection)
        .await
        .map_err(AppError::internal)?;
    let mut tx = connection.begin().await.map_err(AppError::internal)?;
    super::repository::lock_list_writers(&mut tx).await?;
    if let Some(result) =
        existing_command(&mut tx, auth.subject, &run, input.request_id, &request).await?
    {
        tx.rollback().await.map_err(AppError::internal)?;
        return Ok(Json(result));
    }
    let plan = prepare(&mut tx, &run, auth.subject, &input.entry_ids).await?;
    mark_assets(&mut tx, &plan.assets).await?;
    tx.commit().await.map_err(AppError::internal)?;

    if !plan.assets.is_empty() {
        let config = state.test_resources.as_ref().ok_or_else(|| {
            AppError::unavailable(ErrorCode::ServiceUnavailable, "test storage unavailable")
        })?;
        config
            .purge_audio(&run, plan.assets.iter().map(|asset| &asset.object_key))
            .await?;
    }

    let mut tx = connection.begin().await.map_err(AppError::internal)?;
    super::repository::lock_list_writers(&mut tx).await?;
    let current = prepare(&mut tx, &run, auth.subject, &input.entry_ids).await?;
    if current.assets != plan.assets || current.publication_ids != plan.publication_ids {
        return Err(conflict("测试资源在清理期间发生变化，请重新核实后重试"));
    }
    delete_database(&mut tx, &plan).await?;
    let result = TestPurgeResponse {
        run,
        entry_ids: plan.entry_ids,
        publication_ids: plan.publication_ids,
        audio_asset_ids: plan.assets.into_iter().map(|asset| asset.id).collect(),
    };
    sqlx::query(
        "INSERT INTO lexicon_next.test_resource_purge_commands(actor_id,request_id,run_id,request,result) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(auth.subject)
    .bind(input.request_id)
    .bind(&result.run)
    .bind(request)
    .bind(serde_json::to_value(&result).map_err(AppError::internal)?)
    .execute(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_request_rejects_duplicate_ids_and_sorts_exact_ids() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let input = TestPurgeRequest {
            request_id: Uuid::now_v7(),
            entry_ids: vec![second, first],
        };
        let (canonical, _) = canonical_request(&input).unwrap();
        assert_eq!(canonical.entry_ids, vec![first, second]);

        let duplicate = TestPurgeRequest {
            request_id: Uuid::now_v7(),
            entry_ids: vec![first, first],
        };
        assert!(canonical_request(&duplicate).is_err());
    }

    #[test]
    fn canonical_request_rejects_empty_or_nil_requests() {
        assert!(
            canonical_request(&TestPurgeRequest {
                request_id: Uuid::nil(),
                entry_ids: vec![Uuid::now_v7()],
            })
            .is_err()
        );
        assert!(
            canonical_request(&TestPurgeRequest {
                request_id: Uuid::now_v7(),
                entry_ids: vec![],
            })
            .is_err()
        );
    }
}
