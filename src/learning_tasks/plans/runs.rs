//! Open or recover a plan run; selection, source checks and materialization share one transaction.
use super::{latest, meta, units};
use crate::{
    auth::extract::AuthUser,
    error::AppError,
    learning_tasks::{dto::*, mastery, question::*, repository::*, service},
    platform::hash_json as hash,
};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn start(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: StartLearningRun,
) -> Result<LearningRun, AppError> {
    let h = hash(&(id, &input))?;
    let mut receipt = begin(pool, auth, &[]).await?;
    let prior = service::start_receipt(&mut receipt, auth, input.idempotency_key, &h).await?;
    receipt.commit().await.map_err(AppError::internal)?;
    if let Some(run) = prior {
        return service::get_run(pool, auth, run).await;
    }
    let lists = task_lists(pool, auth, id).await?;
    let mut tx = begin(pool, auth, &lists).await?;
    let t = task(&mut tx, auth, id).await?;
    let m = meta(&mut tx, id).await?;
    if m.generation_policy == "legacy_v1" {
        tx.commit().await.map_err(AppError::internal)?;
        return service::start(pool, auth, id, input).await;
    }
    if let Some(run) = service::start_receipt(&mut tx, auth, input.idempotency_key, &h).await? {
        tx.commit().await.map_err(AppError::internal)?;
        return service::get_run(pool, auth, run).await;
    }
    if t.revision != input.expected_revision {
        return Err(conflict("任务版本已变化"));
    }
    let c = latest(&mut tx, id).await?;
    let at = now(&mut tx).await?;
    let (day, window_start, window_end) = business_window(at);
    if t.state != LearningTaskState::Active
        || t.ends_at.is_some_and(|e| e <= at)
        || c.earliest_day > day
    {
        return Err(unavailable());
    }
    let last: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM learning_runs WHERE task_id=$1 ORDER BY started_at DESC,id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    if input.after_run_id.is_some() && input.after_run_id != last {
        return Err(conflict("已有新的练习批次，请刷新后恢复进度"));
    }
    let mut recover = None;
    if let Some(last) = last {
        let mut row = run(&mut tx, auth, last).await?;
        if row.practice_mode.is_some() && row.state == LearningRunState::Active {
            let qs = questions(&mut tx, last).await?;
            let access = sources_for_questions(&mut tx, auth, &lists, &row, &qs).await?;
            let view = run_view(&mut tx, &row, &qs, &access).await?;
            if view.state != row.state {
                sqlx::query("UPDATE learning_runs SET state=$2 WHERE id=$1")
                    .bind(last)
                    .bind(view.state)
                    .execute(&mut *tx)
                    .await
                    .map_err(AppError::internal)?;
                row.state = view.state;
            }
        }
        if row.state == LearningRunState::Active && row.expires_at.is_some_and(|e| e <= at) {
            sqlx::query("UPDATE learning_runs SET state='expired' WHERE id=$1")
                .bind(last)
                .execute(&mut *tx)
                .await
                .map_err(AppError::internal)?;
        } else if row.cycle_id == Some(c.id)
            && (t.task_type == LearningTaskType::Longterm || row.business_day == Some(day))
            && matches!(
                row.state,
                LearningRunState::Active | LearningRunState::Completed
            )
            && (m.generation_policy != mastery::MODE
                || row.state == LearningRunState::Active
                || t.task_type == LearningTaskType::Daily)
        {
            recover = Some(last);
        }
    }
    let run_id = if let Some(r) = recover {
        r
    } else {
        if c.state != "active" {
            return Err(conflict("本轮已结束，请主动开启下一轮"));
        }
        if t.task_type == LearningTaskType::Daily {
            let done:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM learning_completions WHERE task_id=$1 AND business_day=$2)").bind(id).bind(day).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
            if done {
                return Err(conflict("今日任务已完成，请下个学习日继续"));
            }
        }
        let rows = units(&mut tx, c.id).await?;
        let refs: Vec<_> = rows.iter().map(|u| &u.question).collect();
        let access = sources_for_units(&mut tx, auth, &lists, &refs).await?;
        let pending: Vec<_> = rows
            .iter()
            .filter(|u| u.covered_answer_id.is_none() && access.can_practice(&u.question))
            .collect();
        if pending.is_empty() && m.generation_policy != mastery::MODE {
            return Err(conflict("来源不可用，已保留本轮进度，请检查来源"));
        }
        let daily = t.task_type == LearningTaskType::Daily;
        let selected: Vec<Uuid> = if m.generation_policy == mastery::MODE {
            let eligible = mastery::eligible(&mut tx, c.id, day).await?;
            let mut seen = std::collections::HashSet::new();
            eligible
                .into_iter()
                .filter(|id| {
                    rows.iter().find(|u| u.question.id == *id).is_some_and(|u| {
                        access.can_practice(&u.question)
                            && seen.insert((
                                u.question.entry_id,
                                u.question
                                    .custom_item_id
                                    .map(|item| (u.question.source_wordlist_id, item)),
                            ))
                    })
                })
                .take(t.daily_question_count.unwrap_or(20).min(20) as usize)
                .collect()
        } else if daily {
            pending
                .iter()
                .take(t.daily_question_count.expect("daily count") as usize)
                .map(|u| u.question.id)
                .collect()
        } else {
            // Retain source-loss checks during materialization; exclude only invalid question content.
            rows.iter()
                .filter(|u| valid_question(&u.question))
                .map(|u| u.question.id)
                .collect()
        };
        if selected.is_empty() {
            return Err(conflict("本次已无新词或到期复习，请按下次复习日期继续"));
        }
        if t.question_type.as_deref() == Some(crate::question_bank::learning::CHOICE) {
            let selected_units: Vec<_> = rows
                .iter()
                .filter(|u| selected.contains(&u.question.id))
                .map(|u| &u.question)
                .collect();
            let settings =
                serde_json::from_value(c.settings_snapshot.clone()).map_err(AppError::internal)?;
            crate::question_bank::learning::materialize(
                &mut tx,
                &selected_units,
                &settings,
                c.generation_version == WORDLIST_GENERATION_VERSION
                    && !crate::question_bank::learning::is_strict(m.choice_policy.as_deref()),
            )
            .await?;
        }
        let run_id = Uuid::now_v7();
        let expires = daily.then(|| t.ends_at.map_or(window_end, |e| e.min(window_end)));
        sqlx::query("INSERT INTO learning_runs(id,task_id,user_id,task_revision,task_type,business_day,window_start,window_end,expires_at,settings_snapshot,seed,target_count,state,generation_version,grading_version,cycle_id,question_type,practice_mode) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'active',$13,$14,$15,$16,$17)")
  .bind(run_id).bind(id).bind(auth.subject).bind(t.revision).bind(t.task_type).bind(daily.then_some(day)).bind(daily.then_some(window_start)).bind(daily.then_some(window_end)).bind(expires).bind(&c.settings_snapshot).bind(Uuid::now_v7()).bind(selected.len() as i32).bind(&c.generation_version).bind(if c.generation_version == WORDLIST_GENERATION_VERSION && t.question_type.as_deref()==Some(crate::question_bank::learning::CHOICE) {WORDLIST_GRADING_VERSION} else {&c.grading_version}).bind(c.id).bind(t.question_type.as_deref().unwrap_or("spelling_zh_to_en")).bind((m.generation_policy==mastery::MODE).then_some(mastery::MODE)).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("INSERT INTO learning_questions(id,run_id,cycle_id,cycle_unit_id,position,source_wordlist_id,source_revision,source_membership_id,source_public_generation,entry_id,entry_archive_generation,publication_id,pos_id,sense_id,definition_id,form_ids,unit_key,prompt_snapshot,answer_snapshot,fingerprint,custom_item_id,practice_kind) SELECT gen_random_uuid(),$1,cycle_id,id,(row_number() OVER(ORDER BY array_position($2::uuid[],id))-1)::integer,source_wordlist_id,source_revision,source_membership_id,source_public_generation,entry_id,entry_archive_generation,publication_id,pos_id,sense_id,definition_id,form_ids,unit_key,prompt_snapshot,answer_snapshot,fingerprint,custom_item_id,CASE WHEN covered_answer_id IS NULL THEN 'new' ELSE 'review' END FROM learning_cycle_units WHERE id=ANY($2) ORDER BY position").bind(run_id).bind(&selected).execute(&mut *tx).await.map_err(AppError::internal)?;
        run_id
    };
    let row = run(&mut tx, auth, run_id).await?;
    let qs = questions(&mut tx, run_id).await?;
    let access = sources_for_questions(&mut tx, auth, &lists, &row, &qs).await?;
    let ans = answers(&mut tx, run_id).await?;
    if row.state == LearningRunState::Active
        && !qs
            .iter()
            .any(|q| !ans.contains_key(&q.id) && access.can_practice(q))
    {
        return Err(unavailable());
    }
    service::save_start(&mut tx, auth, id, run_id, &input, h).await?;
    learner(&mut tx, auth).await?;
    let end = now(&mut tx).await?;
    if row.state == LearningRunState::Active
        && (row.expires_at.is_some_and(|e| e <= end) || business_window(end).0 < c.earliest_day)
    {
        return Err(unavailable());
    }
    tx.commit().await.map_err(AppError::internal)?;
    service::get_run(pool, auth, run_id).await
}
