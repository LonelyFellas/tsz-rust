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
    #[schema(max_items = 100)]
    pub entry_ids: Vec<Uuid>,
    /// Omit for the legacy entry-only operation; supply the exact set (possibly empty) to close a managed batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(max_items = 100)]
    pub sentence_ids: Option<Vec<Uuid>>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TestPurgeResponse {
    pub run: String,
    pub entry_ids: Vec<Uuid>,
    pub publication_ids: Vec<Uuid>,
    pub audio_asset_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentence_ids: Option<Vec<Uuid>>,
}

#[derive(PartialEq)]
struct AudioAsset {
    id: Uuid,
    object_key: ObjectKey,
}

#[derive(PartialEq)]
struct PurgePlan {
    entry_ids: Vec<Uuid>,
    sentence_ids: Vec<Uuid>,
    publication_ids: Vec<Uuid>,
    assets: Vec<AudioAsset>,
    previews: Vec<(ObjectKey, Vec<u8>)>,
    preview_hashes: Vec<Vec<u8>>,
}

fn conflict(message: &'static str) -> AppError {
    AppError::conflict(ErrorCode::RevisionConflict, None, message)
}

fn canonical_request(input: &TestPurgeRequest) -> Result<(TestPurgeRequest, Value), AppError> {
    let invalid = || {
        AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "test purge request is invalid",
        )
    };
    if input.request_id.is_nil() || (input.entry_ids.is_empty() && input.sentence_ids.is_none()) {
        return Err(invalid());
    }
    let canonical_ids = |ids: &[Uuid]| {
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.len() != ids.len()
            || sorted.len() > MAX_ENTRIES
            || sorted.first().is_some_and(Uuid::is_nil)
        {
            return Err(invalid());
        }
        Ok(sorted)
    };
    let canonical = TestPurgeRequest {
        request_id: input.request_id,
        entry_ids: canonical_ids(&input.entry_ids)?,
        sentence_ids: input
            .sentence_ids
            .as_deref()
            .map(canonical_ids)
            .transpose()?,
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
    sentence_ids: &[Uuid],
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
        "SELECT EXISTS(SELECT 1 FROM lexicon_next.sentence_associations WHERE target_word_id=ANY($1) AND sentence_id<>ALL($2))",
    )
    .bind(entry_ids)
    .bind(sentence_ids)
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
        "SELECT EXISTS (SELECT 1 FROM lexicon_next.test_resource_entries e
                JOIN lexicon_next.test_resource_runs r ON r.run_id=e.run_id
                WHERE e.entry_id=$1 AND r.purge_request_id IS NOT NULL)
            OR (EXISTS (SELECT 1 FROM lexicon_next.test_resource_entries WHERE entry_id=$1)
            AND EXISTS (
                SELECT 1 FROM lexicon.audio_assets a
                WHERE a.reclamation_started_at IS NOT NULL AND a.id IN (
                    SELECT asset_id FROM lexicon_next.audio_asset_references WHERE entry_id=$1
                    UNION ALL
                    SELECT r.asset_id FROM lexicon_next.publications p
                    JOIN lexicon_next.publication_audio_references r ON r.publication_id=p.id
                    WHERE p.entry_id=$1
                )
            ))",
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

async fn check_batch(
    conn: &mut PgConnection,
    run: &str,
    actor: Uuid,
    input: &TestPurgeRequest,
    request: &Value,
) -> Result<(), AppError> {
    let batch: Option<(Uuid, Option<Uuid>, Option<Value>)> = sqlx::query_as(
        "SELECT actor_id,purge_request_id,purge_request FROM lexicon_next.test_resource_runs WHERE run_id=$1 FOR NO KEY UPDATE NOWAIT",
    ).bind(run).fetch_optional(&mut *conn).await.map_err(|error| {
        if error.as_database_error().and_then(|error| error.code()).as_deref() == Some("55P03") {
            return conflict("测试资源有写入正在进行，请稍后重试");
        }
        AppError::internal(error)
    })?;
    let reserved_in_another_run: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon_next.test_resource_runs WHERE actor_id=$1 AND purge_request_id=$2 AND run_id<>$3)")
        .bind(actor).bind(input.request_id).bind(run).fetch_one(&mut *conn).await.map_err(AppError::internal)?;
    if reserved_in_another_run {
        return Err(AppError::conflict(
            ErrorCode::IdempotencyConflict,
            None,
            "请求编号已用于其他测试清理批次",
        ));
    }
    let Some(sentence_ids) = input.sentence_ids.as_ref() else {
        if batch.is_some_and(|(owner, id, _)| owner != actor || id.is_some()) {
            return Err(test_resources::scope_conflict());
        }
        return Ok(());
    };
    let Some((owner, id, original)) = batch else {
        return Err(test_resources::scope_conflict());
    };
    if owner != actor
        || (id.is_some() && (id != Some(input.request_id) || original.as_ref() != Some(request)))
    {
        return Err(test_resources::scope_conflict());
    }
    let entries: Vec<Uuid> = sqlx::query_scalar(
        "SELECT entry_id FROM lexicon_next.test_resource_entries WHERE run_id=$1 ORDER BY entry_id",
    )
    .bind(run)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    let sentences: Vec<Uuid> = sqlx::query_scalar("SELECT s.id FROM lexicon_next.test_resource_sentences r JOIN lexicon_next.sentences s ON s.id=r.sentence_id WHERE r.run_id=$1 AND s.owner_id=$2 ORDER BY s.id FOR UPDATE OF s")
        .bind(run).bind(actor).fetch_all(&mut *conn).await.map_err(AppError::internal)?;
    let foreign_sentences: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon_next.test_resource_sentences r JOIN lexicon_next.sentences s ON s.id=r.sentence_id WHERE r.run_id=$1 AND s.owner_id<>$2)")
        .bind(run).bind(actor).fetch_one(&mut *conn).await.map_err(AppError::internal)?;
    if entries != input.entry_ids || &sentences != sentence_ids || foreign_sentences {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "清理列表必须与专用批次的资源登记完全一致",
        ));
    }
    let external: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM lexicon_next.sentence_references WHERE sentence_id=ANY($1) AND entry_id<>ALL($2)
         UNION ALL SELECT 1 FROM lexicon_next.publications p
         CROSS JOIN LATERAL jsonb_array_elements(COALESCE(p.snapshot->'sentence_resources','[]'::jsonb)) sentence
         WHERE p.entry_id<>ALL($2) AND sentence->>'id'=ANY($3))",
    ).bind(sentence_ids).bind(&input.entry_ids).bind(sentence_ids.iter().map(Uuid::to_string).collect::<Vec<_>>())
        .fetch_one(&mut *conn).await.map_err(AppError::internal)?;
    if external {
        return Err(conflict("测试例句仍被其他词条或历史发布引用"));
    }
    Ok(())
}

async fn prepare(
    conn: &mut PgConnection,
    run: &str,
    actor: Uuid,
    input: &TestPurgeRequest,
) -> Result<PurgePlan, AppError> {
    let entry_ids = &input.entry_ids;
    let sentence_ids = input.sentence_ids.as_deref().unwrap_or_default();
    let managed_run = input.sentence_ids.as_ref().map(|_| run);
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
    ensure_no_external_references(conn, entry_ids, sentence_ids).await?;

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
         WHERE publication.entry_id=ANY($1)
         UNION SELECT asset_id FROM lexicon_next.sentence_audio_asset_references WHERE sentence_id=ANY($2)
         UNION SELECT asset_id FROM lexicon.audio_generations WHERE test_run_id=$3",
    )
    .bind(entry_ids)
    .bind(sentence_ids)
    .bind(managed_run)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    let asset_rows = sqlx::query(
        "SELECT a.id,a.object_key,a.created_by_admin_id,g.asset_id IS NOT NULL AS generated,g.test_run_id,g.created_by_admin_id AS generator FROM lexicon.audio_assets a LEFT JOIN lexicon.audio_generations g ON g.asset_id=a.id WHERE a.id=ANY($1) ORDER BY a.id FOR UPDATE OF a",
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
            return Err(conflict("测试资源包含非当前测试批次的音频对象"));
        }
        if managed_run.is_some()
            && (row.get::<Uuid, _>("created_by_admin_id") != actor
                || (row.get::<bool, _>("generated")
                    && (row.get::<Option<&str>, _>("test_run_id") != managed_run
                        || row.get::<Option<Uuid>, _>("generator") != Some(actor))))
        {
            return Err(conflict("测试批次中包含未登记或不属于当前管理员的音频资产"));
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
            WHERE r.asset_id=ANY($1) AND r.sentence_id<>ALL($3)
            UNION ALL
            SELECT 1 FROM lexicon_next.publication_audio_references r
            WHERE r.asset_id=ANY($1)
              AND (r.publication_id<>ALL($4) OR ($5 AND r.resource_id<>ALL($2) AND r.resource_id<>ALL($3)))
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
    .bind(sentence_ids)
    .bind(&publication_ids)
    .bind(managed_run.is_some())
    .fetch_one(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if external_asset_reference {
        return Err(conflict("音频资产仍被清理批次之外的数据引用"));
    }

    // Managed generation holds a shared run lock for its entire detached task.
    // After acquiring the exclusive batch lock, any managed pending row is interrupted.
    if managed_run.is_none() {
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM lexicon.audio_generations WHERE asset_id=ANY($1) AND state='pending')",
        ).bind(&asset_ids).fetch_one(&mut *conn).await.map_err(AppError::internal)?;
        if pending {
            return Err(conflict("音频生成任务仍在进行，暂不能 purge"));
        }
    }
    let preview_rows: Vec<(String, Vec<u8>)> = sqlx::query_as("SELECT object_key,request_hash FROM lexicon_next.test_resource_previews WHERE run_id=$1 ORDER BY object_key")
        .bind(managed_run).fetch_all(&mut *conn).await.map_err(AppError::internal)?;
    let previews = preview_rows
        .into_iter()
        .map(|(key, hash)| Ok((ObjectKey::parse(key).map_err(AppError::internal)?, hash)))
        .collect::<Result<_, AppError>>()?;
    let preview_hashes = sqlx::query_scalar("SELECT request_hash FROM lexicon_next.test_resource_preview_requests WHERE run_id=$1 UNION SELECT request_hash FROM lexicon_next.test_resource_previews WHERE run_id=$1 ORDER BY request_hash")
        .bind(managed_run).fetch_all(&mut *conn).await.map_err(AppError::internal)?;
    Ok(PurgePlan {
        entry_ids: entry_ids.to_vec(),
        sentence_ids: sentence_ids.to_vec(),
        publication_ids,
        assets,
        previews,
        preview_hashes,
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

async fn delete_database(
    conn: &mut PgConnection,
    plan: &PurgePlan,
    managed_run: Option<&str>,
) -> Result<(), AppError> {
    let asset_ids = plan.assets.iter().map(|asset| asset.id).collect::<Vec<_>>();
    let semantic_keys:Vec<(i64,String)>=sqlx::query_as(
        "SELECT DISTINCT d.dataset_id,d.source_key FROM learning_source.next_choice_candidates c
         JOIN dictionary.entry_contents d ON d.normalized_term=lexicon.question_candidate_key_v1(c.spelling)
         WHERE c.publication_id=ANY($1) ORDER BY d.dataset_id,d.source_key"
    ).bind(&plan.publication_ids).fetch_all(&mut *conn).await.map_err(AppError::internal)?;
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

    for (dataset, key) in semantic_keys {
        sqlx::query("SELECT dictionary.refresh_choice_semantics($1,$2)")
            .bind(dataset)
            .bind(key)
            .execute(&mut *conn)
            .await
            .map_err(AppError::internal)?;
    }
    sqlx::query("DELETE FROM lexicon_next.audio_asset_references WHERE entry_id=ANY($1)")
        .bind(&plan.entry_ids)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("DELETE FROM lexicon_next.sentence_references WHERE entry_id=ANY($1) AND sentence_id=ANY($2)")
        .bind(&plan.entry_ids).bind(&plan.sentence_ids).execute(&mut *conn).await.map_err(AppError::internal)?;
    sqlx::query("DELETE FROM lexicon_next.sentences WHERE id=ANY($1)")
        .bind(&plan.sentence_ids)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    if let Some(run) = managed_run {
        sqlx::query("DELETE FROM speech.preview_cache c USING lexicon_next.test_resource_previews p WHERE p.run_id=$1 AND c.object_key=p.object_key AND c.request_hash=p.request_hash")
            .bind(run).execute(&mut *conn).await.map_err(AppError::internal)?;
        sqlx::query("DELETE FROM lexicon_next.test_resource_previews WHERE run_id=$1")
            .bind(run)
            .execute(&mut *conn)
            .await
            .map_err(AppError::internal)?;
        sqlx::query("DELETE FROM lexicon_next.test_resource_preview_requests WHERE run_id=$1")
            .bind(run)
            .execute(&mut *conn)
            .await
            .map_err(AppError::internal)?;
    }
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
    if input
        .sentence_ids
        .as_ref()
        .is_some_and(|ids| !ids.is_empty())
    {
        authorization.require("sentences.access")?;
        authorization.require("sentences.create")?;
    }
    // Serialize both legacy and batch purges per dedicated actor across storage I/O.
    // The list-writer lock is released before waiting for storage.
    let command_lock = PgAdvisoryLock::new(format!("lexicon_next.purge/{}", auth.subject));
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
    check_batch(&mut tx, &run, auth.subject, &input, &request).await?;
    let plan = prepare(&mut tx, &run, auth.subject, &input).await?;
    let managed_run = input.sentence_ids.as_ref().map(|_| run.as_str());
    if managed_run.is_some() {
        sqlx::query("UPDATE lexicon_next.test_resource_runs SET purge_request_id=$2,purge_request=$3 WHERE run_id=$1")
            .bind(&run).bind(input.request_id).bind(&request).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    mark_assets(&mut tx, &plan.assets).await?;
    tx.commit().await.map_err(AppError::internal)?;

    if !plan.assets.is_empty() || managed_run.is_some() {
        let config = state.test_resources.as_ref().ok_or_else(|| {
            AppError::unavailable(ErrorCode::ServiceUnavailable, "test storage unavailable")
        })?;
        let audio_keys = plan.assets.iter().map(|asset| &asset.object_key);
        if managed_run.is_some() {
            config
                .purge_run_objects(
                    &run,
                    audio_keys.chain(plan.previews.iter().map(|(key, _)| key)),
                )
                .await?;
        } else {
            config.purge_audio(&run, audio_keys).await?;
        }
    }
    if !plan.preview_hashes.is_empty() {
        crate::speech::preview::clear_test_locks(
            &state.redis,
            plan.preview_hashes.iter().map(Vec::as_slice),
        )
        .await?;
    }

    let mut tx = connection.begin().await.map_err(AppError::internal)?;
    super::repository::lock_list_writers(&mut tx).await?;
    check_batch(&mut tx, &run, auth.subject, &input, &request).await?;
    let current = prepare(&mut tx, &run, auth.subject, &input).await?;
    if current != plan {
        return Err(conflict("测试资源在清理期间发生变化，请重新核实后重试"));
    }
    delete_database(&mut tx, &plan, managed_run).await?;
    let result = TestPurgeResponse {
        run,
        entry_ids: plan.entry_ids,
        publication_ids: plan.publication_ids,
        audio_asset_ids: plan.assets.into_iter().map(|asset| asset.id).collect(),
        sentence_ids: input.sentence_ids,
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
            sentence_ids: Some(vec![second, first]),
        };
        let (canonical, _) = canonical_request(&input).unwrap();
        assert_eq!(canonical.entry_ids, vec![first, second]);

        let duplicate = TestPurgeRequest {
            request_id: Uuid::now_v7(),
            entry_ids: vec![first, first],
            sentence_ids: None,
        };
        assert!(canonical_request(&duplicate).is_err());
    }

    #[test]
    fn canonical_request_rejects_empty_or_nil_requests() {
        assert!(
            canonical_request(&TestPurgeRequest {
                request_id: Uuid::nil(),
                entry_ids: vec![Uuid::now_v7()],
                sentence_ids: None,
            })
            .is_err()
        );
        assert!(
            canonical_request(&TestPurgeRequest {
                request_id: Uuid::now_v7(),
                entry_ids: vec![],
                sentence_ids: None,
            })
            .is_err()
        );
    }
}
