use super::{conflict, dto::*, invalid, service, source};
use crate::{
    admin::{AdminAuth, permissions::AdminAuthorization},
    error::AppError,
    learning_tasks::repository::Tx,
    wordlists::service::{hash, page_meta},
};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, types::Json};
use std::{collections::HashSet, sync::Arc};
use uuid::Uuid;

// All creation paths take this lock before testing coverage/inserting tasks.
// It also bounds worker admission across processes; no network I/O is done under it.
pub(super) async fn lock_creation(tx: &mut Tx<'_>) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(2810281200)")
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    Ok(())
}
fn normalize(mut s: QuestionBatchSelection) -> Result<QuestionBatchSelection, AppError> {
    if s.question_levels.is_empty()
        || s.question_levels.len() > 6
        || s.answer_profiles.is_empty()
        || s.answer_profiles.len() > 2
    {
        return Err(invalid("请选择一至六个等级和一至两个英美档位"));
    }
    for l in &s.question_levels {
        for p in &s.answer_profiles {
            source::validate_context(l, p)?;
        }
    }
    s.question_levels.sort();
    s.question_levels.dedup();
    s.answer_profiles.sort();
    s.answer_profiles.dedup();
    Ok(s)
}
async fn all_candidates(
    tx: &mut Tx<'_>,
    auth: &AdminAuthorization,
    q: &QuestionCandidateQuery,
) -> Result<Vec<QuestionCandidate>, AppError> {
    source::validate_context(&q.question_level, &q.answer_profile)?;
    if q.query.as_ref().is_some_and(|v| v.chars().count() > 100) {
        return Err(invalid("检索词过长"));
    }
    let entries: Vec<(Uuid,Uuid)>=sqlx::query_as("SELECT e.id,e.created_by_admin_id FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.archived_at IS NULL AND e.teaching_eligible AND p.content_schema_version=3 AND ($1::uuid IS NULL OR e.id=$1) AND ($2::text IS NULL OR p.snapshot::text ILIKE '%'||$2||'%') ORDER BY e.id FOR SHARE OF e")
        .bind(q.entry_id).bind(q.query.as_deref().filter(|s|!s.is_empty())).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let ids: Vec<_> = entries
        .into_iter()
        .filter(|(_, owner)| auth.require_owned_action("words.edit", *owner).is_ok())
        .map(|(id, _)| id)
        .collect();
    let mut result = Vec::new();
    for chunk in ids.chunks(64) {
        result.extend(source::candidates(tx, chunk, &q.question_level, &q.answer_profile).await?);
    }
    result.sort_by(|a, b| {
        a.source
            .entry_id
            .cmp(&b.source.entry_id)
            .then(a.source.pos_id.cmp(&b.source.pos_id))
            .then(a.source.sense_id.cmp(&b.source.sense_id))
            .then(a.source.definition_id.cmp(&b.source.definition_id))
            .then(a.source.form_id.cmp(&b.source.form_id))
    });
    Ok(result)
}
pub async fn candidate_page(
    pool: &PgPool,
    auth: &AdminAuth,
    q: QuestionCandidatePageQuery,
) -> Result<QuestionCandidateSearchPage, AppError> {
    let (p, s) = service::pagination(&QuestionBankQuery {
        page: q.page,
        page_size: q.page_size,
        state: None,
    })?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let a = service::authorize(&mut tx, auth, "words.access", None).await?;
    let all = all_candidates(
        &mut tx,
        &a,
        &QuestionCandidateQuery {
            entry_id: q.entry_id,
            query: q.query,
            question_level: q.question_level,
            answer_profile: q.answer_profile,
        },
    )
    .await?;
    let total = all.len() as i64;
    let items = all
        .into_iter()
        .skip(((u64::from(p) - 1) * u64::from(s)).min(usize::MAX as u64) as usize)
        .take(s as usize)
        .collect();
    tx.commit().await.map_err(AppError::internal)?;
    Ok(QuestionCandidateSearchPage {
        items,
        pagination: page_meta(p, s, total),
    })
}
fn identity(c: &QuestionCandidate) -> String {
    format!(
        "{}:{}:{}:{}",
        c.source.entry_id, c.source.publication_id, c.source.sense_id, c.source.definition_id
    )
}
struct Missing {
    target: Arc<QuestionCandidate>,
    level: String,
}
async fn plan(
    tx: &mut Tx<'_>,
    a: &AdminAuthorization,
    selection: QuestionBatchSelection,
) -> Result<(QuestionBatchPreview, Vec<Missing>), AppError> {
    let selection = normalize(selection)?;
    let active:Vec<(String,String,String)>=sqlx::query_as("SELECT (target#>>'{source,entry_id}')||':'||(target#>>'{source,publication_id}')||':'||(target#>>'{source,sense_id}')||':'||(target#>>'{source,definition_id}'),question_level,answer_profile FROM question_generations WHERE state IN ('queued','running')").fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let active: HashSet<_> = active.into_iter().collect();
    let questions: Vec<Json<BankQuestionContent>> = sqlx::query_scalar(
        "SELECT content FROM question_bank WHERE state IN ('draft','approved') AND question_level=ANY($1) AND answer_profile=ANY($2) ORDER BY id",
    ).bind(&selection.question_levels).bind(&selection.answer_profiles)
    .fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    // Recheck published snapshots once per entry/profile, rather than once per option
    // of every question in the whole bank. Saved questions already passed validation.
    let mut ids: Vec<_> = questions
        .iter()
        .flat_map(|q| {
            q.options
                .iter()
                .map(|o| o.candidate.source.entry_id)
                .chain(std::iter::once(q.target.source.entry_id))
        })
        .collect();
    ids.sort();
    ids.dedup();
    let mut current = HashSet::new();
    for profile in &selection.answer_profiles {
        for chunk in ids.chunks(64) {
            current.extend(
                source::candidates(tx, chunk, "C2", profile)
                    .await?
                    .into_iter()
                    .map(|c| c.content_fingerprint),
            );
        }
    }
    let mut covered = HashSet::new();
    for Json(c) in questions {
        if current.contains(&c.target.content_fingerprint)
            && c.options
                .iter()
                .all(|o| current.contains(&o.candidate.content_fingerprint))
        {
            covered.insert((identity(&c.target), c.question_level, c.answer_profile));
        }
    }
    let mut coverage = Vec::new();
    let mut missing = Vec::new();
    for profile in &selection.answer_profiles {
        let all = all_candidates(
            tx,
            a,
            &QuestionCandidateQuery {
                entry_id: None,
                query: None,
                question_level: "C2".into(),
                answer_profile: profile.clone(),
            },
        )
        .await?;
        let all: Vec<_> = all.into_iter().map(Arc::new).collect();
        for level in &selection.question_levels {
            let mut row = QuestionBatchCoverage {
                question_level: level.clone(),
                answer_profile: profile.clone(),
                eligible: 0,
                covered: 0,
                generating: 0,
                missing: 0,
            };
            let mut seen = HashSet::new();
            for c in &all {
                if !seen.insert(identity(c)) {
                    continue;
                }
                // CEFR codes are lexically ordered A1..C2.
                if c.level > *level {
                    continue;
                }
                row.eligible += 1;
                let key = (identity(c), level.clone(), profile.clone());
                if covered.contains(&key) {
                    row.covered += 1;
                } else if active.contains(&key) {
                    row.generating += 1;
                } else {
                    row.missing += 1;
                    missing.push(Missing {
                        target: Arc::clone(c),
                        level: level.clone(),
                    });
                }
            }
            coverage.push(row);
        }
    }
    let identities: Vec<_> = missing
        .iter()
        .map(|m| {
            (
                &m.target.content_fingerprint,
                &m.level,
                &m.target.answer_profile,
            )
        })
        .collect();
    let preview_token = hash(&(&selection, identities))?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((
        QuestionBatchPreview {
            selection,
            coverage,
            missing: missing.len() as i64,
            preview_token,
        },
        missing,
    ))
}
pub async fn preview(
    pool: &PgPool,
    auth: &AdminAuth,
    s: QuestionBatchSelection,
) -> Result<QuestionBatchPreview, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let a = service::authorize(&mut tx, auth, "words.edit", None).await?;
    let (preview, _) = plan(&mut tx, &a, s).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(preview)
}
pub async fn create(
    pool: &PgPool,
    auth: &AdminAuth,
    mut input: CreateQuestionBatch,
) -> Result<QuestionBatch, AppError> {
    input.selection = normalize(input.selection)?;
    let digest = hash(&input)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let a = service::authorize(&mut tx, auth, "words.edit", None).await?;
    lock_creation(&mut tx).await?;
    let old:Option<(Uuid,Vec<u8>)>=sqlx::query_as("SELECT id,request_hash FROM question_generation_batches WHERE created_by=$1 AND request_key=$2").bind(auth.subject).bind(input.idempotency_key).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    let id = if let Some((id, old)) = old {
        if old != digest {
            return Err(conflict("请求键已用于另一批次"));
        }
        id
    } else {
        let (preview, missing) = plan(&mut tx, &a, input.selection.clone()).await?;
        if preview.preview_token != input.preview_token {
            return Err(conflict("词库或覆盖情况已变化，请重新预览后创建批次"));
        }
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO question_generation_batches(id,created_by,request_key,request_hash,selection,preview) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(auth.subject).bind(input.idempotency_key).bind(digest).bind(Json(&input.selection)).bind(Json(preview)).execute(&mut *tx).await.map_err(AppError::internal)?;
        let rows = missing
            .iter()
            .map(|m| Ok((Uuid::now_v7(), hash(m.target.as_ref())?, m)))
            .collect::<Result<Vec<_>, AppError>>()?;
        for chunk in rows.chunks(250) {
            let mut insert = sqlx::QueryBuilder::new(
                "INSERT INTO question_generations(id,created_by,security_version,permission_version,request_key,request_hash,target,question_level,answer_profile,batch_id) ",
            );
            insert.push_values(chunk, |mut b, (gid, digest, m)| {
                b.push_bind(*gid)
                    .push_bind(auth.subject)
                    .push_bind(auth.security_version)
                    .push_bind(a.permission_version)
                    .push_bind(*gid)
                    .push_bind(digest)
                    .push_bind(Json(m.target.as_ref()))
                    .push_bind(&m.level)
                    .push_bind(&m.target.answer_profile)
                    .push_bind(id);
            });
            insert
                .build()
                .execute(&mut *tx)
                .await
                .map_err(AppError::internal)?;
        }
        id
    };
    let result = view(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
async fn view(tx: &mut Tx<'_>, id: Uuid) -> Result<QuestionBatch, AppError> {
    let (created_by,Json(preview),cancelled_at,created_at):(Uuid,Json<QuestionBatchPreview>,Option<DateTime<Utc>>,DateTime<Utc>)=sqlx::query_as("SELECT created_by,preview,cancelled_at,created_at FROM question_generation_batches WHERE id=$1").bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?.ok_or_else(||AppError::not_found("批次不存在"))?;
    let counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT state,count(*) FROM question_generations WHERE batch_id=$1 GROUP BY state",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    let count = |state: &str| {
        counts
            .iter()
            .find(|(s, _)| s == state)
            .map_or(0, |(_, n)| *n)
    };
    let queued = count("queued");
    let running = count("running");
    Ok(QuestionBatch {
        id,
        created_by,
        preview,
        state: if cancelled_at.is_some() {
            "cancelled"
        } else if queued + running > 0 {
            "running"
        } else {
            "completed"
        }
        .into(),
        total: counts.iter().map(|(_, n)| n).sum(),
        queued,
        running,
        succeeded: count("succeeded"),
        failed: count("failed"),
        insufficient_candidates: count("insufficient_candidates"),
        cancelled: count("cancelled"),
        created_at,
    })
}
pub async fn get(pool: &PgPool, auth: &AdminAuth, id: Uuid) -> Result<QuestionBatch, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::authorize(&mut tx, auth, "words.access", None).await?;
    let r = view(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(r)
}
pub async fn list(
    pool: &PgPool,
    auth: &AdminAuth,
    q: QuestionBankQuery,
) -> Result<QuestionBatchPage, AppError> {
    let (p, s) = service::pagination(&q)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::authorize(&mut tx, auth, "words.access", None).await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM question_generation_batches")
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM question_generation_batches ORDER BY created_at DESC,id DESC LIMIT $1 OFFSET $2").bind(i64::from(s)).bind(i64::from(p-1)*i64::from(s)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let mut items = Vec::new();
    for id in ids {
        items.push(view(&mut tx, id).await?);
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(QuestionBatchPage {
        items,
        pagination: page_meta(p, s, total),
    })
}
pub async fn cancel(pool: &PgPool, auth: &AdminAuth, id: Uuid) -> Result<QuestionBatch, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    // Authorize before taking task locks, matching create/worker lock order.
    let owner: Uuid =
        sqlx::query_scalar("SELECT created_by FROM question_generation_batches WHERE id=$1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(AppError::internal)?
            .ok_or_else(|| AppError::not_found("批次不存在"))?;
    service::authorize(&mut tx, auth, "words.edit", Some(owner)).await?;
    sqlx::query("UPDATE question_generation_batches SET cancelled_at=COALESCE(cancelled_at,now()) WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    sqlx::query("UPDATE question_generations SET state='cancelled',error_code=NULL,reasons='{}',cancel_delivered=(submitted_at IS NULL),revision=revision+1,updated_at=now(),next_attempt_at=now() WHERE batch_id=$1 AND state IN ('queued','running')").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    let r = view(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(r)
}
pub async fn items(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    q: QuestionBankQuery,
) -> Result<QuestionGenerationPage, AppError> {
    let (p, s) = service::pagination(&q)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::authorize(&mut tx, auth, "words.access", None).await?;
    view(&mut tx, id).await?;
    let total:i64=sqlx::query_scalar("SELECT count(*) FROM question_generations WHERE batch_id=$1 AND ($2::text IS NULL OR state=$2)").bind(id).bind(&q.state).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    let rows:Vec<service::GenerationRow>=sqlx::query_as("SELECT * FROM question_generations WHERE batch_id=$1 AND ($2::text IS NULL OR state=$2) ORDER BY id LIMIT $3 OFFSET $4").bind(id).bind(&q.state).bind(i64::from(s)).bind(i64::from(p-1)*i64::from(s)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(QuestionGenerationPage {
        items: rows.into_iter().map(service::GenerationRow::view).collect(),
        pagination: page_meta(p, s, total),
    })
}
