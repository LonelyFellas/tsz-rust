use super::{dto::*, question::*, repository::*};
use crate::{
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
    wordlists::service::{hash, page_meta},
};
use sqlx::PgPool;
use std::collections::HashSet;
use uuid::Uuid;
fn validate(
    kind: LearningTaskType,
    ids: &[Uuid],
    n: Option<i32>,
    ends: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(), AppError> {
    if ids.is_empty() || ids.len() > 5 || ids.iter().collect::<HashSet<_>>().len() != ids.len() {
        return Err(invalid("请选择 1–5 个不重复词表"));
    }
    if (kind == LearningTaskType::Daily && !n.is_some_and(|n| (1..=200).contains(&n)))
        || (kind == LearningTaskType::Longterm && (n.is_some() || ends.is_some()))
    {
        return Err(invalid("每日题量须为 1–200；长期任务不设每日题量和截止"));
    }
    Ok(())
}
fn name(value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() || value.chars().count() > 100 {
        return Err(invalid("名称须为 1–100 字"));
    }
    Ok(())
}
fn match_hash(old: &[u8], new: &[u8]) -> Result<(), AppError> {
    if old != new {
        return Err(AppError::conflict(
            ErrorCode::IdempotencyConflict,
            None,
            "请求键已用于不同内容",
        ));
    }
    Ok(())
}
async fn pool_for(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    ids: &[Uuid],
) -> Result<(CandidatePool, i64, LearningContext), AppError> {
    let context = learner(tx, auth).await?;
    let access = sources(tx, auth, ids).await?;
    if access.lists.len() != ids.len() {
        return Err(unavailable());
    }
    let entries: HashSet<_> = access.ordered.iter().map(|(_, e)| e).collect();
    if entries.len() > 1000 {
        return Err(invalid("来源超过 1000 个词条，请拆分任务"));
    }
    let mut candidates = build_candidates(&access.learning_sources()?, &context)?;
    candidates.exclusions.unavailable_entries = entries
        .iter()
        .filter(|e| !access.entries.contains_key(e))
        .count() as i64;
    if candidates.candidates.len() > 2000 {
        return Err(invalid("候选超过 2000 题，请拆分任务"));
    }
    Ok((candidates, entries.len() as i64, context))
}
pub async fn preview(
    pool: &PgPool,
    auth: &AuthUser,
    input: PreviewLearningTask,
) -> Result<LearningPreview, AppError> {
    validate(
        input.task_type,
        &input.wordlist_ids,
        input.daily_question_count,
        input.ends_at,
    )?;
    let mut tx = begin(pool, auth, &input.wordlist_ids).await?;
    let (candidates, entry_count, settings) = pool_for(&mut tx, auth, &input.wordlist_ids).await?;
    let eligible_count = candidates.candidates.len() as i64;
    let at = now(&mut tx).await?;
    let can_start = eligible_count >= i64::from(input.daily_question_count.unwrap_or(1))
        && input.ends_at.is_none_or(|t| t > at);
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(LearningPreview {
        eligible_count,
        entry_count,
        settings,
        exclusions: candidates.exclusions,
        can_start,
    })
}
pub async fn create(
    pool: &PgPool,
    auth: &AuthUser,
    input: CreateLearningTask,
) -> Result<LearningTask, AppError> {
    name(&input.name)?;
    validate(
        input.task_type,
        &input.wordlist_ids,
        input.daily_question_count,
        input.ends_at,
    )?;
    let h = hash(&input)?;
    // No source dependency for a committed creation receipt.
    let mut receipt = begin(pool, auth, &[]).await?;
    let prior: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT id,create_hash FROM learning_tasks WHERE user_id=$1 AND create_key=$2",
    )
    .bind(auth.subject)
    .bind(input.idempotency_key)
    .fetch_optional(&mut *receipt)
    .await
    .map_err(AppError::internal)?;
    if let Some((id, old)) = prior {
        match_hash(&old, &h)?;
        let result = task(&mut receipt, auth, id).await?;
        receipt.commit().await.map_err(AppError::internal)?;
        return Ok(result);
    }
    receipt.commit().await.map_err(AppError::internal)?;
    let mut tx = begin(pool, auth, &input.wordlist_ids).await?;
    let prior: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT id,create_hash FROM learning_tasks WHERE user_id=$1 AND create_key=$2",
    )
    .bind(auth.subject)
    .bind(input.idempotency_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let id = if let Some((id, old)) = prior {
        match_hash(&old, &h)?;
        id
    } else {
        let at = now(&mut tx).await?;
        if input.ends_at.is_some_and(|t| t <= at) {
            return Err(invalid("结束时间必须晚于当前时间"));
        }
        let access = sources(&mut tx, auth, &input.wordlist_ids).await?;
        if access.lists.len() != input.wordlist_ids.len() {
            return Err(unavailable());
        }
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO learning_tasks(id,user_id,name,task_type,wordlist_ids,daily_question_count,ends_at,create_key,create_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(id).bind(auth.subject).bind(input.name.trim()).bind(input.task_type).bind(&input.wordlist_ids).bind(input.daily_question_count).bind(input.ends_at).bind(input.idempotency_key).bind(h).execute(&mut *tx).await.map_err(AppError::internal)?;
        id
    };
    let result = task(&mut tx, auth, id).await?;
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn rename(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: RenameLearningTask,
) -> Result<LearningTask, AppError> {
    name(&input.name)?;
    let mut tx = begin(pool, auth, &[]).await?;
    let t = task(&mut tx, auth, id).await?;
    if t.revision != input.expected_revision {
        return Err(conflict("任务版本已变化"));
    }
    sqlx::query("UPDATE learning_tasks SET name=$2,revision=revision+1 WHERE id=$1")
        .bind(id)
        .bind(input.name.trim())
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let result = task(&mut tx, auth, id).await?;
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn archive(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: ArchiveLearningTask,
) -> Result<LearningTask, AppError> {
    let mut tx = begin(pool, auth, &[]).await?;
    let t = task(&mut tx, auth, id).await?;
    if t.revision != input.expected_revision {
        return Err(conflict("任务版本已变化"));
    }
    sqlx::query("UPDATE learning_tasks SET state='archived',revision=revision+1 WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("UPDATE learning_runs SET state='cancelled' WHERE task_id=$1 AND state='active'")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let result = task(&mut tx, auth, id).await?;
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
async fn start_receipt(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    key: Uuid,
    h: &[u8],
) -> Result<Option<Uuid>, AppError> {
    let row:Option<(Uuid,Vec<u8>)>=sqlx::query_as("SELECT run_id,request_hash FROM learning_run_start_requests WHERE user_id=$1 AND request_key=$2").bind(auth.subject).bind(key).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
    if let Some((id, old)) = row {
        match_hash(&old, h)?;
        Ok(Some(id))
    } else {
        Ok(None)
    }
}
async fn save_start(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    task_id: Uuid,
    id: Uuid,
    input: &StartLearningRun,
    h: Vec<u8>,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO learning_run_start_requests(user_id,request_key,task_id,request_hash,run_id) VALUES($1,$2,$3,$4,$5)").bind(auth.subject).bind(input.idempotency_key).bind(task_id).bind(h).bind(id).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
pub async fn start(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: StartLearningRun,
) -> Result<LearningRun, AppError> {
    let h = hash(&(id, &input))?;
    let mut receipt = begin(pool, auth, &[]).await?;
    let prior = start_receipt(&mut receipt, auth, input.idempotency_key, &h).await?;
    receipt.commit().await.map_err(AppError::internal)?;
    if let Some(run) = prior {
        return get_run(pool, auth, run).await;
    }
    let lists = task_lists(pool, auth, id).await?;
    let mut tx = begin(pool, auth, &lists).await?;
    let t = task(&mut tx, auth, id).await?;
    if let Some(run) = start_receipt(&mut tx, auth, input.idempotency_key, &h).await? {
        tx.commit().await.map_err(AppError::internal)?;
        return get_run(pool, auth, run).await;
    }
    if t.revision != input.expected_revision {
        return Err(conflict("任务版本已变化"));
    }
    let at = now(&mut tx).await?;
    if t.state != LearningTaskState::Active || t.ends_at.is_some_and(|e| e <= at) {
        return Err(unavailable());
    }
    let (day, window_start, window_end) = business_window(at);
    let latest: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM learning_runs WHERE task_id=$1 ORDER BY started_at DESC,id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let mut recover = None;
    if let Some(last) = latest {
        let row = run(&mut tx, auth, last).await?;
        let qs = questions(&mut tx, last).await?;
        let access = sources_for_questions(&mut tx, auth, &lists, &qs).await?;
        let view = run_view(&mut tx, &row, &qs, &access).await?;
        if view.state != row.state {
            sqlx::query("UPDATE learning_runs SET state=$2 WHERE id=$1")
                .bind(last)
                .bind(view.state)
                .execute(&mut *tx)
                .await
                .map_err(AppError::internal)?;
        }
        let same_day = t.task_type == LearningTaskType::Longterm || row.business_day == Some(day);
        if same_day
            && (view.state == LearningRunState::Active
                || (t.task_type == LearningTaskType::Daily
                    && view.state == LearningRunState::Completed))
        {
            recover = Some(last);
        } else if (same_day || input.after_run_id.is_some()) && input.after_run_id != Some(last) {
            tx.commit().await.map_err(AppError::internal)?;
            return Err(conflict("请明确选择从最新轮次重新开始"));
        }
        if recover.is_some() && input.after_run_id.is_some() && input.after_run_id != Some(last) {
            return Err(conflict("已有后继轮次，请恢复当前进度"));
        }
    } else if input.after_run_id.is_some() {
        return Err(conflict("前驱轮次不存在"));
    }
    let run_id = if let Some(run) = recover {
        run
    } else {
        let (mut candidates, _, settings) = match pool_for(&mut tx, auth, &lists).await {
            Ok(result) => result,
            Err(error) => {
                tx.commit().await.map_err(AppError::internal)?;
                return Err(error);
            }
        };
        let count = t
            .daily_question_count
            .unwrap_or(candidates.candidates.len() as i32);
        if count == 0 || candidates.candidates.len() < (count as usize) {
            tx.commit().await.map_err(AppError::internal)?;
            return Err(AppError::conflict(
                ErrorCode::LearningInsufficientQuestions,
                None,
                format!(
                    "仅有 {} 道可出题内容，需要 {count} 道；请查看预览排除原因",
                    candidates.candidates.len()
                ),
            ));
        }
        let seed = Uuid::now_v7();
        if t.task_type == LearningTaskType::Daily {
            candidates.candidates.sort_by_cached_key(|c| {
                hash(&(seed, &c.unit_key)).expect("serializable candidate key")
            });
        }
        let run = Uuid::now_v7();
        let daily = t.task_type == LearningTaskType::Daily;
        let expires = if daily {
            Some(t.ends_at.map_or(window_end, |e| e.min(window_end)))
        } else {
            None
        };
        sqlx::query("INSERT INTO learning_runs(id,task_id,user_id,task_revision,task_type,business_day,window_start,window_end,expires_at,settings_snapshot,seed,target_count,state,generation_version,grading_version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'active',$13,$14)").bind(run).bind(id).bind(auth.subject).bind(t.revision).bind(t.task_type).bind(daily.then_some(day)).bind(daily.then_some(window_start)).bind(daily.then_some(window_end)).bind(expires).bind(serde_json::to_value(settings).map_err(AppError::internal)?).bind(seed).bind(count).bind(GENERATION_VERSION).bind(GRADING_VERSION).execute(&mut *tx).await.map_err(AppError::internal)?;
        for (position, c) in candidates
            .candidates
            .into_iter()
            .take(count as usize)
            .enumerate()
        {
            sqlx::query("INSERT INTO learning_questions(id,run_id,position,source_wordlist_id,source_revision,entry_id,entry_archive_generation,publication_id,pos_id,sense_id,definition_id,form_ids,unit_key,prompt_snapshot,answer_snapshot,fingerprint,source_membership_id,source_public_generation) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)").bind(Uuid::now_v7()).bind(run).bind(position as i32).bind(c.source_wordlist_id).bind(c.source_revision).bind(c.entry_id).bind(c.entry_archive_generation).bind(c.publication_id).bind(c.pos_id).bind(c.sense_id).bind(c.definition_id).bind(&c.form_ids).bind(&c.unit_key).bind(serde_json::to_value(&c.prompt).map_err(AppError::internal)?).bind(serde_json::to_value(&c.answer).map_err(AppError::internal)?).bind(hash(&c)?).bind(c.source_membership_id).bind(c.source_public_generation).execute(&mut *tx).await.map_err(AppError::internal)?;
        }
        let time = now(&mut tx).await?;
        if expires.is_some_and(|e| e <= time) {
            return Err(unavailable());
        }
        run
    };
    save_start(&mut tx, auth, id, run_id, &input, h).await?;
    let final_run = run(&mut tx, auth, run_id).await?;
    let fixed = questions(&mut tx, run_id).await?;
    let final_access = sources_for_questions(&mut tx, auth, &lists, &fixed).await?;
    if final_run.state == LearningRunState::Active
        && fixed.iter().any(|q| !final_access.available(q))
    {
        return Err(unavailable());
    }
    learner(&mut tx, auth).await?;
    let time = now(&mut tx).await?;
    if final_run.state == LearningRunState::Active
        && final_run.expires_at.is_some_and(|e| e <= time)
    {
        return Err(unavailable());
    }
    tx.commit().await.map_err(AppError::internal)?;
    get_run(pool, auth, run_id).await
}
pub async fn get_run(pool: &PgPool, auth: &AuthUser, id: Uuid) -> Result<LearningRun, AppError> {
    let task_id = run_task(pool, auth, id).await?;
    let lists = task_lists(pool, auth, task_id).await?;
    let mut tx = begin(pool, auth, &lists).await?;
    task(&mut tx, auth, task_id).await?;
    let row = run(&mut tx, auth, id).await?;
    let qs = questions(&mut tx, id).await?;
    let access = sources_for_questions(&mut tx, auth, &lists, &qs).await?;
    let result = run_view(&mut tx, &row, &qs, &access).await?;
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn get_questions(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    query: LearningPageQuery,
) -> Result<LearningQuestionPage, AppError> {
    let (p, s) = page(&query)?;
    let task_id = run_task(pool, auth, id).await?;
    let lists = task_lists(pool, auth, task_id).await?;
    let mut tx = begin(pool, auth, &lists).await?;
    task(&mut tx, auth, task_id).await?;
    run(&mut tx, auth, id).await?;
    let qs = questions(&mut tx, id).await?;
    let access = sources_for_questions(&mut tx, auth, &lists, &qs).await?;
    let ans = answers(&mut tx, id).await?;
    let total = qs.len() as i64;
    let items = qs
        .iter()
        .skip((u64::from(p - 1) * u64::from(s)) as usize)
        .take(s as usize)
        .map(|q| question_view(q, ans.get(&q.id), &access))
        .collect::<Result<_, _>>()?;
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(LearningQuestionPage {
        items,
        pagination: page_meta(p, s, total),
    })
}
async fn answer_receipt(
    tx: &mut Tx<'_>,
    run_id: Uuid,
    key: Uuid,
    h: &[u8],
) -> Result<Option<Uuid>, AppError> {
    let row: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT id,request_hash FROM learning_answers WHERE run_id=$1 AND request_key=$2",
    )
    .bind(run_id)
    .bind(key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    if let Some((id, old)) = row {
        match_hash(&old, h)?;
        Ok(Some(id))
    } else {
        Ok(None)
    }
}
async fn receipt_view(
    pool: &PgPool,
    auth: &AuthUser,
    run_id: Uuid,
    answer_id: Uuid,
) -> Result<LearningAnswerReceipt, AppError> {
    let task_id = run_task(pool, auth, run_id).await?;
    let lists = task_lists(pool, auth, task_id).await?;
    let mut tx = begin(pool, auth, &lists).await?;
    task(&mut tx, auth, task_id).await?;
    let row = run(&mut tx, auth, run_id).await?;
    let qs = questions(&mut tx, run_id).await?;
    let access = sources_for_questions(&mut tx, auth, &lists, &qs).await?;
    let ans = answers(&mut tx, run_id).await?;
    let a = ans
        .values()
        .find(|a| a.id == answer_id)
        .ok_or_else(|| AppError::not_found("作答不存在"))?;
    let q = qs
        .iter()
        .find(|q| q.id == a.question_id)
        .ok_or_else(|| AppError::not_found("题目不存在"))?;
    let result = LearningAnswerReceipt {
        answer_id: a.id,
        question_id: q.id,
        is_correct: a.is_correct,
        accepted_at: a.accepted_at,
        run: run_view(&mut tx, &row, &qs, &access).await?,
        question: question_view(q, Some(a), &access)?,
    };
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn answer(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: SubmitLearningAnswer,
) -> Result<LearningAnswerReceipt, AppError> {
    let h = hash(&input)?;
    let mut receipt = begin(pool, auth, &[]).await?;
    // Ownership is checked before looking at the key/hash.
    let owned: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM learning_runs WHERE id=$1 AND user_id=$2)")
            .bind(id)
            .bind(auth.subject)
            .fetch_one(&mut *receipt)
            .await
            .map_err(AppError::internal)?;
    if !owned {
        return Err(AppError::not_found("轮次不存在"));
    }
    let prior = answer_receipt(&mut receipt, id, input.idempotency_key, &h).await?;
    receipt.commit().await.map_err(AppError::internal)?;
    if let Some(a) = prior {
        return receipt_view(pool, auth, id, a).await;
    }
    let task_id = run_task(pool, auth, id).await?;
    let lists = task_lists(pool, auth, task_id).await?;
    let mut tx = begin(pool, auth, &lists).await?;
    let t = task(&mut tx, auth, task_id).await?;
    let row = run(&mut tx, auth, id).await?;
    if let Some(a) = answer_receipt(&mut tx, id, input.idempotency_key, &h).await? {
        tx.commit().await.map_err(AppError::internal)?;
        return receipt_view(pool, auth, id, a).await;
    }
    let qs = questions(&mut tx, id).await?;
    let q = qs
        .iter()
        .find(|q| q.id == input.question_id)
        .ok_or_else(|| AppError::not_found("题目不存在"))?;
    let ans = answers(&mut tx, id).await?;
    if ans.contains_key(&q.id) {
        return Err(AppError::conflict(
            ErrorCode::LearningAlreadyAnswered,
            None,
            "该题已记录首次作答，请恢复服务器进度",
        ));
    }
    let snapshot: AnswerSnapshot =
        serde_json::from_value(q.answer_snapshot.clone()).map_err(AppError::internal)?;
    let (normalized, correct) = grade_spelling(&snapshot, &input.answer)?;
    let access = sources_for_questions(&mut tx, auth, &lists, &qs).await?;
    let view = run_view(&mut tx, &row, &qs, &access).await?;
    if view.state != LearningRunState::Active
        || t.state != LearningTaskState::Active
        || row.grading_version != GRADING_VERSION
    {
        if view.state != row.state {
            sqlx::query("UPDATE learning_runs SET state=$2 WHERE id=$1")
                .bind(id)
                .bind(view.state)
                .execute(&mut *tx)
                .await
                .map_err(AppError::internal)?;
            tx.commit().await.map_err(AppError::internal)?;
        }
        return Err(unavailable());
    }
    let at = now(&mut tx).await?;
    let answer_id = Uuid::now_v7();
    sqlx::query("INSERT INTO learning_answers(id,run_id,question_id,request_key,request_hash,submitted_answer,normalized_answer,is_correct,accepted_at,grading_version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)").bind(answer_id).bind(id).bind(q.id).bind(input.idempotency_key).bind(h).bind(&input.answer).bind(normalized).bind(correct).bind(at).bind(&row.grading_version).execute(&mut *tx).await.map_err(AppError::internal)?;
    let (answered, correct_count): (i64, i64) = sqlx::query_as(
        "SELECT count(*),count(*) FILTER(WHERE is_correct) FROM learning_answers WHERE run_id=$1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    if answered == i64::from(row.target_count) {
        sqlx::query("INSERT INTO learning_completions(id,run_id,task_id,user_id,task_type,business_day,task_revision,target_count,answered_count,correct_count,completed_at,generation_version,grading_version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)").bind(Uuid::now_v7()).bind(id).bind(task_id).bind(auth.subject).bind(row.task_type).bind(row.business_day).bind(row.task_revision).bind(row.target_count).bind(answered as i32).bind(correct_count as i32).bind(at).bind(&row.generation_version).bind(&row.grading_version).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("UPDATE learning_runs SET state='completed' WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    }
    // Source owners can cross their deletion deadline while locks are held.
    let final_access = sources_for_questions(&mut tx, auth, &lists, &qs).await?;
    if qs.iter().any(|q| !final_access.available(q)) {
        return Err(unavailable());
    }
    learner(&mut tx, auth).await?;
    let end = now(&mut tx).await?;
    if row.expires_at.is_some_and(|e| e <= end) {
        return Err(unavailable());
    }
    tx.commit().await.map_err(AppError::internal)?;
    receipt_view(pool, auth, id, answer_id).await
}
pub async fn detail(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
) -> Result<LearningTaskDetail, AppError> {
    let mut tx = begin(pool, auth, &[]).await?;
    let t = task(&mut tx, auth, id).await?;
    let at = now(&mut tx).await?;
    let day = business_window(at).0;
    let current:Option<Uuid>=sqlx::query_scalar("SELECT id FROM learning_runs WHERE task_id=$1 AND ($2='longterm' OR business_day=$3) ORDER BY started_at DESC,id DESC LIMIT 1").bind(id).bind(t.task_type).bind(day).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    let current_run = match current {
        Some(id) => Some(get_run(pool, auth, id).await?),
        None => None,
    };
    Ok(LearningTaskDetail {
        task: t,
        server_time: at,
        business_day: day,
        timezone: "Asia/Shanghai".into(),
        current_run,
    })
}
pub async fn list(
    pool: &PgPool,
    auth: &AuthUser,
    query: LearningPageQuery,
) -> Result<LearningTaskPage, AppError> {
    let (p, s) = page(&query)?;
    let mut tx = begin(pool, auth, &[]).await?;
    let state = query.state.unwrap_or(LearningTaskState::Active);
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM learning_tasks WHERE user_id=$1 AND state=$2")
            .bind(auth.subject)
            .bind(state)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM learning_tasks WHERE user_id=$1 AND state=$2 ORDER BY created_at DESC,id DESC LIMIT $3 OFFSET $4").bind(auth.subject).bind(state).bind(i64::from(s)).bind(i64::from(p-1)*i64::from(s)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    let mut items = vec![];
    for id in ids {
        items.push(detail(pool, auth, id).await?);
    }
    Ok(LearningTaskPage {
        items,
        pagination: page_meta(p, s, total),
    })
}
pub async fn history(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    query: LearningPageQuery,
) -> Result<LearningRunPage, AppError> {
    let (p, s) = page(&query)?;
    let mut tx = begin(pool, auth, &[]).await?;
    task(&mut tx, auth, id).await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_runs WHERE task_id=$1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM learning_runs WHERE task_id=$1 ORDER BY started_at DESC,id DESC LIMIT $2 OFFSET $3").bind(id).bind(i64::from(s)).bind(i64::from(p-1)*i64::from(s)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    let mut items = vec![];
    for id in ids {
        items.push(get_run(pool, auth, id).await?);
    }
    Ok(LearningRunPage {
        items,
        pagination: page_meta(p, s, total),
    })
}
