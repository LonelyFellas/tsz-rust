//! Answer replay and submission; progress, retries, rewards and final source checks commit atomically.
use super::match_hash;
use crate::{
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
    learning_tasks::{dto::*, mastery, plans, question::*, repository::*, rewards},
    platform::hash_json as hash,
};
use sqlx::PgPool;
use uuid::Uuid;

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
    let access = sources_for_questions(&mut tx, auth, &lists, &row, &qs).await?;
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
    invitation_rewards_enabled: bool,
    memory_enabled: bool,
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
    if row.practice_mode.is_some()
        && qs
            .iter()
            .find(|q| !ans.contains_key(&q.id))
            .is_none_or(|next| next.id != q.id)
    {
        return Err(conflict("请按顺序完成当前题目后继续重练"));
    }
    let (normalized, correct) = if row.practice_mode.is_some() && input.answer == mastery::UNKNOWN {
        (input.answer.clone(), false)
    } else if snapshot.choice.is_some() {
        crate::question_bank::learning::grade(&snapshot, &input.answer)?
    } else {
        grade_spelling(&snapshot, &input.answer)?
    };
    let access = sources_for_questions(&mut tx, auth, &lists, &row, &qs).await?;
    let view = run_view(&mut tx, &row, &qs, &access).await?;
    if view.state != LearningRunState::Active
        || t.state != LearningTaskState::Active
        || !access.can_practice(q)
        || row.grading_version
            != if row.generation_version == WORDLIST_GENERATION_VERSION
                && row.question_type == crate::question_bank::learning::CHOICE
            {
                WORDLIST_GRADING_VERSION
            } else if row.question_type == crate::question_bank::learning::CHOICE {
                crate::question_bank::learning::GRADING
            } else {
                GRADING_VERSION
            }
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
    sqlx::query("INSERT INTO learning_answers(id,run_id,question_id,request_key,request_hash,submitted_answer,normalized_answer,is_correct,accepted_at,grading_version,cycle_unit_id,is_repeat) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)").bind(answer_id).bind(id).bind(q.id).bind(input.idempotency_key).bind(h).bind(&input.answer).bind(normalized).bind(correct).bind(at).bind(if snapshot.choice.is_some() {crate::question_bank::learning::GRADING} else {GRADING_VERSION}).bind(q.cycle_unit_id).bind(q.practice_kind!="new").execute(&mut *tx).await.map_err(AppError::internal)?;
    plans::cover(&mut tx, q.cycle_unit_id, answer_id).await?;
    let mut target_count = row.target_count;
    if row.practice_mode.is_some() {
        mastery::record(&mut tx, q, correct).await?;
        let remaining = qs
            .iter()
            .filter(|other| {
                other.id != q.id && !ans.contains_key(&other.id) && access.can_practice(other)
            })
            .count();
        if !correct && mastery::retry(&mut tx, &row, q, remaining).await? {
            target_count += 1;
        }
    }
    let (answered, correct_count): (i64, i64) = sqlx::query_as(
        "SELECT count(*),count(*) FILTER(WHERE is_correct) FROM learning_answers WHERE run_id=$1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let mut completed = false;
    if answered == i64::from(target_count) {
        completed = true;
        let completion_id = Uuid::now_v7();
        sqlx::query("INSERT INTO learning_completions(id,run_id,task_id,user_id,task_type,business_day,task_revision,target_count,answered_count,correct_count,completed_at,generation_version,grading_version,question_type) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)").bind(completion_id).bind(id).bind(task_id).bind(auth.subject).bind(row.task_type).bind(row.business_day).bind(row.task_revision).bind(target_count).bind(answered as i32).bind(correct_count as i32).bind(at).bind(&row.generation_version).bind(&row.grading_version).bind(&row.question_type).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("UPDATE learning_runs SET state='completed' WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        rewards::settle_completion_in(&mut tx, completion_id).await?;
        crate::invitations::service::settle_qualified_in(
            &mut tx,
            completion_id,
            auth.subject,
            invitation_rewards_enabled,
        )
        .await?;
    }
    // Source owners can cross their deletion deadline while locks are held.
    let final_access = sources_for_questions(&mut tx, auth, &lists, &row, &qs).await?;
    if !final_access.can_practice(q)
        || (row.cycle_id.is_none() && qs.iter().any(|q| !final_access.available(q)))
    {
        return Err(unavailable());
    }
    // A run kept alive only by a deleted source ends with this completion. Archive
    // the task in the same transaction so no later run can be started from it.
    if completed && !access.grace.is_empty() {
        sqlx::query(
            "UPDATE learning_tasks SET state='archived',revision=revision+1 WHERE id=$1 AND state='active'",
        )
        .bind(task_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        sqlx::query(
            "UPDATE learning_runs SET state='cancelled' WHERE task_id=$1 AND state='active' AND id<>$2",
        )
        .bind(task_id)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    }
    if completed && !access.grace.is_empty() {
        plans::cancel_cycles(&mut tx, task_id).await?;
    }
    if memory_enabled && snapshot.choice.is_none() && q.practice_kind != "retry" {
        crate::learning_memory::record_answer(
            &mut tx,
            auth.subject,
            &row,
            q,
            answer_id,
            &final_access,
        )
        .await?;
    }
    learner(&mut tx, auth).await?;
    let end = now(&mut tx).await?;
    if row.expires_at.is_some_and(|e| e <= end) {
        return Err(unavailable());
    }
    tx.commit().await.map_err(AppError::internal)?;
    receipt_view(pool, auth, id, answer_id).await
}
