//! Plan creation and cycle operations, reusing task/run/answer facts.
//! Views live in `queries`, run scheduling in `runs`, and source rebinding in `source_refresh`.
//! Main record: tsz/docs/features/task-spelling-creation/.
pub mod dto;
pub mod handler;
mod queries;
mod runs;
mod source_refresh;

use super::{dto::*, question::*, repository::*, service};
use crate::{auth::extract::AuthUser, error::AppError, platform::hash_json as hash};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use dto::*;
use queries::{cycle_by_id, summarize};
pub use queries::{cycles, detail, history, list};
pub use runs::start;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct PlanMeta {
    generation_policy: String,
    choice_policy: Option<String>,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
}
#[derive(sqlx::FromRow)]
struct CycleRow {
    id: Uuid,
    cycle_number: i32,
    state: String,
    total_count: i32,
    settings_snapshot: serde_json::Value,
    generation_version: String,
    grading_version: String,
    earliest_day: NaiveDate,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}
#[derive(sqlx::FromRow)]
struct Unit {
    #[sqlx(flatten)]
    question: QuestionRow,
    covered_answer_id: Option<Uuid>,
    practice_success_days: i16,
}
async fn meta(tx: &mut Tx<'_>, id: Uuid) -> Result<PlanMeta, AppError> {
    sqlx::query_as("SELECT generation_policy,choice_policy,start_date,end_date FROM learning_tasks WHERE id=$1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)
}
async fn latest(tx: &mut Tx<'_>, id: Uuid) -> Result<CycleRow, AppError> {
    sqlx::query_as("SELECT * FROM learning_cycles WHERE task_id=$1 ORDER BY cycle_number DESC LIMIT 1 FOR UPDATE").bind(id).fetch_one(&mut **tx).await.map_err(AppError::internal)
}
async fn units(tx: &mut Tx<'_>, id: Uuid) -> Result<Vec<Unit>, AppError> {
    sqlx::query_as("SELECT *,id AS cycle_unit_id,'new' AS practice_kind FROM learning_cycle_units WHERE cycle_id=$1 ORDER BY position").bind(id).fetch_all(&mut **tx).await.map_err(AppError::internal)
}
fn schedule(m: &PlanMeta) -> Option<LearningSchedule> {
    m.start_date
        .zip(m.end_date)
        .map(|(start_date, end_date)| LearningSchedule {
            start_date,
            end_date,
        })
}
fn boundary(d: NaiveDate) -> DateTime<Utc> {
    d.and_hms_opt(0, 0, 0).expect("midnight").and_utc() - Duration::hours(4)
}
fn digest(value: &impl serde::Serialize) -> Result<String, AppError> {
    Ok(hash(value)?.iter().map(|b| format!("{b:02x}")).collect())
}
fn validate(input: &PreviewLearningPlan) -> Result<(), AppError> {
    if input.choice_policy.as_deref().is_some_and(|p| {
        ![
            crate::question_bank::learning::STRICT_POLICY,
            crate::question_bank::learning::MIXED_POLICY,
        ]
        .contains(&p)
            || input.question_type.as_deref() != Some(crate::question_bank::learning::CHOICE)
    }) {
        return Err(invalid("严格单选策略只适用于单选任务"));
    }
    if input
        .practice_mode
        .as_deref()
        .is_some_and(|m| m != super::mastery::MODE)
    {
        return Err(invalid("练习模式无效"));
    }
    if input.practice_mode.is_some() && input.daily_question_count.is_some_and(|n| n > 20) {
        return Err(invalid(
            "每次新学和到期复习最多 20 题，含重练总计最多 30 题",
        ));
    }
    if input
        .question_type
        .as_deref()
        .is_some_and(|t| !["spelling_zh_to_en", "choice_zh_to_en"].contains(&t))
    {
        return Err(invalid("题型无效"));
    }
    service::validate(
        input.task_type,
        &input.wordlist_ids,
        input.daily_question_count,
        None,
    )?;
    if input.task_type == LearningTaskType::Longterm && input.schedule.is_some() {
        return Err(invalid("长期任务不设日期计划"));
    }
    Ok(())
}
fn plan_name(value: &str) -> Result<(), AppError> {
    if !(1..=50).contains(&value.trim().chars().count()) {
        return Err(invalid("名称须为 1–50 字"));
    }
    Ok(())
}
fn ceil(r: i64, n: i64) -> i64 {
    (r + n - 1) / n
}
pub fn remaining_days(r: i64, n: i64, a: i64) -> i64 {
    if r == 0 {
        0
    } else if a > 0 {
        1 + ceil((r - a).max(0), n)
    } else {
        ceil(r, n)
    }
}
async fn preview_in(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    input: &PreviewLearningPlan,
    earliest: Option<NaiveDate>,
) -> Result<(LearningPlanPreview, Vec<Candidate>), AppError> {
    validate(input)?;
    let (pool, entry_count, settings) = service::pool_for_plan(
        tx,
        auth,
        &input.wordlist_ids,
        input.question_type.as_deref() == Some(crate::question_bank::learning::CHOICE),
        input.choice_policy.as_deref(),
    )
    .await?;
    let custom_word_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wordlist_items WHERE wordlist_id=ANY($1) AND entry_id IS NULL",
    )
    .bind(&input.wordlist_ids)
    .fetch_one(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    let at = now(tx).await?;
    let day = business_window(at).0;
    let count = pool.candidates.len() as i64;
    let estimated = if input.practice_mode.is_some() {
        None
    } else {
        input
            .daily_question_count
            .map(|n| ceil(count, i64::from(n)))
    };
    let mut reasons = vec![];
    if count == 0 {
        reasons.push(
            if crate::question_bank::learning::is_strict(input.choice_policy.as_deref())
                && pool
                    .choice_preview
                    .as_ref()
                    .is_some_and(|preview| preview.total_count > 0)
            {
                "所选内容暂不可出单选题，请查看暂不可用内容与原因"
            } else {
                "词表中没有可出题内容，请检查中文释义和英文词面"
            }
            .into(),
        );
    }
    let available = input.schedule.as_ref().map(|s| {
        if s.start_date < day && earliest.is_none() {
            reasons.push("开始日期不得早于当前学习日".into());
        }
        if s.end_date < s.start_date {
            reasons.push("结束日期不得早于开始日期".into());
        }
        let first = s.start_date.max(earliest.unwrap_or(day));
        let days = (s.end_date - first).num_days() + 1;
        if estimated.is_some_and(|n| days < n) {
            reasons.push("日期计划容量不足，请调整日期或新建任务".into());
        }
        days.max(0)
    });
    let preview_fingerprint = if input.choice_policy.is_some() {
        digest(&(
            input,
            &pool.candidates,
            &pool.choice_preview,
            &settings,
            earliest,
            crate::question_bank::learning::STRATEGY_VERSION,
        ))?
    } else {
        digest(&(input, &pool.candidates, &settings, earliest))?
    };
    learner(tx, auth).await?;
    Ok((
        LearningPlanPreview {
            choice: pool.choice_preview,
            entry_count,
            eligible_count: count,
            custom_word_count,
            exclusions: pool.exclusions,
            settings,
            estimated_learning_days: estimated,
            available_learning_days: available,
            can_create: reasons.is_empty(),
            reasons,
            preview_fingerprint,
            server_time: at,
            business_day: day,
            timezone: "Asia/Shanghai".into(),
        },
        pool.candidates,
    ))
}
pub async fn preview(
    pool: &PgPool,
    auth: &AuthUser,
    input: PreviewLearningPlan,
) -> Result<LearningPlanPreview, AppError> {
    let mut tx = begin(pool, auth, &input.wordlist_ids).await?;
    let (p, _) = preview_in(&mut tx, auth, &input, None).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(p)
}
async fn write_cycle(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    task_id: Uuid,
    predecessor: Option<&CycleRow>,
    earliest: NaiveDate,
    preview: &LearningPlanPreview,
    candidates: &[Candidate],
) -> Result<Uuid, AppError> {
    let id = Uuid::now_v7();
    let choice = candidates.iter().any(|c| c.answer.choice.is_some());
    sqlx::query("INSERT INTO learning_cycles(id,task_id,user_id,cycle_number,state,total_count,settings_snapshot,generation_version,grading_version,after_cycle_id,earliest_day) VALUES($1,$2,$3,$4,'active',$5,$6,$7,$8,$9,$10)").bind(id).bind(task_id).bind(auth.subject).bind(predecessor.map_or(1, |c| c.cycle_number+1)).bind(candidates.len() as i32).bind(serde_json::to_value(&preview.settings).map_err(AppError::internal)?).bind(WORDLIST_GENERATION_VERSION).bind(if choice {WORDLIST_GRADING_VERSION} else {GRADING_VERSION}).bind(predecessor.map(|c|c.id)).bind(earliest).execute(&mut **tx).await.map_err(AppError::internal)?;
    for (position, c) in candidates.iter().enumerate() {
        let mut answer = c.answer.clone();
        if let Some(choice) = answer.choice.as_mut() {
            choice.deferred = true;
            choice.content.options.clear();
            choice.content.correct_option_id.clear();
            choice.content.explanation.clear();
        }
        sqlx::query("INSERT INTO learning_cycle_units(id,cycle_id,position,source_wordlist_id,source_revision,source_membership_id,source_public_generation,entry_id,entry_archive_generation,publication_id,pos_id,sense_id,definition_id,form_ids,unit_key,prompt_snapshot,answer_snapshot,fingerprint,custom_item_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)")
   .bind(Uuid::now_v7()).bind(id).bind(position as i32).bind(c.source_wordlist_id).bind(c.source_revision).bind(c.source_membership_id).bind(c.source_public_generation).bind(c.entry_id).bind(c.entry_archive_generation).bind(c.publication_id).bind(c.pos_id).bind(c.sense_id).bind(c.definition_id).bind(&c.form_ids).bind(&c.unit_key).bind(serde_json::to_value(&c.prompt).map_err(AppError::internal)?).bind(serde_json::to_value(&answer).map_err(AppError::internal)?).bind(hash(c)?).bind(c.custom_item_id).execute(&mut **tx).await.map_err(AppError::internal)?;
    }
    Ok(id)
}
pub async fn create(
    pool: &PgPool,
    auth: &AuthUser,
    input: CreateLearningPlan,
) -> Result<LearningPlan, AppError> {
    let h = hash(&input)?;
    let mut receipt = begin(pool, auth, &[]).await?;
    let prior = service::create_receipt(&mut receipt, auth, input.idempotency_key, &h).await?;
    receipt.commit().await.map_err(AppError::internal)?;
    if let Some(id) = prior {
        return detail(pool, auth, id).await;
    }
    plan_name(&input.name)?;
    validate(&input.preview())?;
    if input.reuse_active
        && (input.task_type != LearningTaskType::Longterm
            || input.wordlist_ids.len() != 1
            || input.schedule.is_some())
    {
        return Err(invalid("词表续练仅支持单份词表的长期练习"));
    }
    let mut tx = begin(pool, auth, &input.wordlist_ids).await?;
    if let Some(id) = service::create_receipt(&mut tx, auth, input.idempotency_key, &h).await? {
        tx.commit().await.map_err(AppError::internal)?;
        return detail(pool, auth, id).await;
    }
    let (p, candidates) = preview_in(&mut tx, auth, &input.preview(), None).await?;
    if p.preview_fingerprint != input.preview_fingerprint {
        return Err(conflict("题量、来源或学习设置已变化，请重新核对预览后创建"));
    }
    if p.choice.as_ref().is_some_and(|c| c.unavailable_count > 0) && !input.accept_partial_choice {
        return Err(invalid(
            "部分内容暂不可出单选，请核对预览并确认仅使用可用内容创建",
        ));
    }
    if !p.can_create {
        return Err(invalid(&p.reasons.join("；")).with_detail_template(
            crate::i18n::errors::DetailTemplate::LearningPlanReasons(p.reasons),
        ));
    }
    let direct_key = if input.reuse_active {
        let mut scope: Vec<_> = candidates
            .iter()
            .cloned()
            .map(|mut candidate| {
                candidate.answer.choice = None;
                candidate
            })
            .collect();
        // Choice availability can reorder the pool without changing its learning targets.
        scope.sort_by(|a, b| a.unit_key.cmp(&b.unit_key));
        Some(hash(&(&input.preview(), &p.settings, &scope))?)
    } else {
        None
    };
    if let Some(key) = &direct_key {
        let prior: Option<Uuid> = sqlx::query_scalar("SELECT id FROM learning_tasks WHERE user_id=$1 AND state='active' AND direct_practice_key=$2 FOR UPDATE")
            .bind(auth.subject).bind(key).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
        if let Some(id) = prior {
            sqlx::query("INSERT INTO learning_plan_reuse_receipts(user_id,request_key,request_hash,task_id) VALUES($1,$2,$3,$4)")
                .bind(auth.subject).bind(input.idempotency_key).bind(&h).bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
            tx.commit().await.map_err(AppError::internal)?;
            return detail(pool, auth, id).await;
        }
    }
    let id = Uuid::now_v7();
    let s = input.schedule.as_ref();
    sqlx::query("INSERT INTO learning_tasks(id,user_id,name,task_type,wordlist_ids,daily_question_count,ends_at,create_key,create_hash,generation_policy,start_date,end_date,question_type,choice_policy) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$13,$10,$11,$12,$14)")
 .bind(id).bind(auth.subject).bind(input.name.trim()).bind(input.task_type).bind(&input.wordlist_ids).bind(input.daily_question_count).bind(s.map(|s|boundary(s.end_date+Duration::days(1)))).bind(input.idempotency_key).bind(h).bind(s.map(|s|s.start_date)).bind(s.map(|s|s.end_date)).bind(input.question_type.as_deref().unwrap_or("spelling_zh_to_en")).bind(input.practice_mode.as_deref().unwrap_or("coverage_v1")).bind(input.choice_policy.as_deref()).execute(&mut *tx).await.map_err(AppError::internal)?;
    if let Some(key) = direct_key {
        sqlx::query("UPDATE learning_tasks SET direct_practice_key=$2 WHERE id=$1")
            .bind(id)
            .bind(key)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    }
    let cycle_id = write_cycle(
        &mut tx,
        auth,
        id,
        None,
        s.map_or(p.business_day, |s| s.start_date),
        &p,
        &candidates,
    )
    .await?;
    learner(&mut tx, auth).await?;
    ensure_cycle_sources(&mut tx, auth, &input.wordlist_ids, cycle_id).await?;
    // Recompute only time-dependent validation after source/entry locks.
    let end = business_window(now(&mut tx).await?).0;
    if s.is_some_and(|s| s.start_date < end) {
        return Err(conflict("学习日已变化，请核对日期后重试"));
    }
    tx.commit().await.map_err(AppError::internal)?;
    detail(pool, auth, id).await
}
async fn ensure_cycle_sources(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    lists: &[Uuid],
    id: Uuid,
) -> Result<(), AppError> {
    let rows = units(tx, id).await?;
    let refs: Vec<_> = rows.iter().map(|u| &u.question).collect();
    let access = super::repository::sources_for_units(tx, auth, lists, &refs).await?;
    if rows.iter().any(|u| !access.can_practice(&u.question)) {
        return Err(unavailable());
    }
    Ok(())
}
pub async fn require_legacy(
    pool: &PgPool,
    auth: &AuthUser,
    id: Option<Uuid>,
) -> Result<(), AppError> {
    let mut tx = begin(pool, auth, &[]).await?;
    require_legacy_in(&mut tx, auth, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(())
}
pub(super) async fn require_legacy_in(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    id: Option<Uuid>,
) -> Result<(), AppError> {
    let has:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM learning_tasks WHERE user_id=$1 AND ($2::uuid IS NULL OR id=$2) AND generation_policy<>'legacy_v1' AND ($2::uuid IS NOT NULL OR question_type='spelling_zh_to_en'))").bind(auth.subject).bind(id).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
    if has {
        return Err(conflict("任务功能已更新，请刷新页面后继续"));
    }
    Ok(())
}
pub(super) async fn cancel_cycles(tx: &mut Tx<'_>, id: Uuid) -> Result<(), AppError> {
    sqlx::query("UPDATE learning_cycles SET state='cancelled' WHERE task_id=$1 AND state='active'")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    Ok(())
}
pub(super) async fn cover(
    tx: &mut Tx<'_>,
    unit: Option<Uuid>,
    answer: Uuid,
) -> Result<(), AppError> {
    if let Some(unit) = unit {
        let cycle:Option<Uuid>=sqlx::query_scalar("UPDATE learning_cycle_units SET covered_answer_id=$2 WHERE id=$1 AND covered_answer_id IS NULL RETURNING cycle_id").bind(unit).bind(answer).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
        let Some(cycle) = cycle else {
            return Ok(());
        };
        sqlx::query("UPDATE learning_cycles SET state='completed',completed_at=clock_timestamp() WHERE id=$1 AND state='active' AND NOT EXISTS(SELECT 1 FROM learning_tasks t WHERE t.id=learning_cycles.task_id AND t.generation_policy='mastery_v1') AND NOT EXISTS(SELECT 1 FROM learning_cycle_units WHERE cycle_id=$1 AND covered_answer_id IS NULL)").bind(cycle).execute(&mut **tx).await.map_err(AppError::internal)?;
    }
    Ok(())
}
pub async fn rename(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: RenameLearningTask,
) -> Result<LearningPlan, AppError> {
    let mut tx = begin(pool, auth, &[]).await?;
    task(&mut tx, auth, id).await?;
    if meta(&mut tx, id).await?.generation_policy != "legacy_v1" {
        plan_name(&input.name)?;
    }
    tx.commit().await.map_err(AppError::internal)?;
    service::rename(pool, auth, id, input).await?;
    detail(pool, auth, id).await
}
pub async fn archive(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: ArchiveLearningTask,
) -> Result<LearningPlan, AppError> {
    service::archive(pool, auth, id, input).await?;
    detail(pool, auth, id).await
}
async fn cycle_preview_in(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    t: &LearningTask,
    m: &PlanMeta,
    input: &PreviewLearningCycle,
) -> Result<(LearningCyclePreview, Vec<Candidate>, CycleRow), AppError> {
    if input.operation == CycleOperation::RefreshSources {
        return source_refresh::preview_in(tx, auth, t, m, input).await;
    }
    if m.generation_policy != "coverage_v1" {
        return Err(conflict("此模式不支持整轮重建；掌握任务请重新确认来源"));
    }
    let c = latest(tx, t.id).await?;
    if c.id != input.after_cycle_id || t.revision != input.expected_revision {
        return Err(conflict("任务或当前轮次已变化，请刷新"));
    }
    let rows = units(tx, c.id).await?;
    let current_id:Option<Uuid>=sqlx::query_scalar("SELECT id FROM learning_runs WHERE cycle_id=$1 AND state='active' ORDER BY started_at DESC LIMIT 1").bind(c.id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
    let rr = if let Some(id) = current_id {
        Some(run(tx, auth, id).await?)
    } else {
        None
    };
    let old = summarize(tx, auth, t, &c, &rows, rr.as_ref()).await?;
    if (input.operation == CycleOperation::Next && c.state != "completed")
        || (input.operation == CycleOperation::RestartBlocked
            && (c.state != "active" || old.blocked_count == 0))
    {
        return Err(conflict("当前轮次不允许此操作"));
    }
    let at = now(tx).await?;
    let day = business_window(at).0;
    if t.state != LearningTaskState::Active || t.ends_at.is_some_and(|e| e <= at) {
        return Err(unavailable());
    }
    let mut earliest = day;
    if t.task_type == LearningTaskType::Daily {
        if input.operation == CycleOperation::Next {
            earliest = day.max(
                business_window(c.completed_at.expect("completed timestamp")).0 + Duration::days(1),
            );
        } else {
            let used:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM learning_answers a JOIN learning_runs r ON r.id=a.run_id WHERE r.task_id=$1 AND a.accepted_at >= $2 AND a.accepted_at < $3) OR EXISTS(SELECT 1 FROM learning_completions WHERE task_id=$1 AND business_day=$4)").bind(t.id).bind(boundary(day)).bind(boundary(day+Duration::days(1))).bind(day).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
            if used {
                earliest = day + Duration::days(1);
            }
        }
        earliest = earliest.max(m.start_date.unwrap_or(day));
    }
    let req = PreviewLearningPlan {
        choice_policy: m.choice_policy.clone(),
        practice_mode: None,
        question_type: t.question_type.clone(),
        task_type: t.task_type,
        wordlist_ids: t.wordlist_ids.clone(),
        daily_question_count: t.daily_question_count,
        schedule: schedule(m),
    };
    let (p, cs) = preview_in(tx, auth, &req, Some(earliest)).await?;
    if p.business_day != day {
        return Err(conflict("学习日已变化，请重新核对新轮计划"));
    }
    Ok((
        LearningCyclePreview {
            source_refresh: None,
            preview: p,
            earliest_day: earliest,
            previous_covered_count: old.covered_count,
            previous_total_count: old.total_count,
            previous_blocked_count: old.blocked_count,
        },
        cs,
        c,
    ))
}
pub async fn cycle_preview(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: PreviewLearningCycle,
) -> Result<LearningCyclePreview, AppError> {
    let lists = task_lists(pool, auth, id).await?;
    let mut tx = begin(pool, auth, &lists).await?;
    let t = task(&mut tx, auth, id).await?;
    let m = meta(&mut tx, id).await?;
    let (p, _, _) = cycle_preview_in(&mut tx, auth, &t, &m, &input).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(p)
}
async fn cycle_receipt(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    key: Uuid,
    h: &[u8],
) -> Result<Option<Uuid>, AppError> {
    let r:Option<(Uuid,Vec<u8>)>=sqlx::query_as("SELECT result_cycle_id,request_hash FROM learning_cycle_requests WHERE user_id=$1 AND idempotency_key=$2").bind(auth.subject).bind(key).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
    if let Some((id, old)) = r {
        service::match_hash(&old, h)?;
        Ok(Some(id))
    } else {
        Ok(None)
    }
}
pub async fn cycle_create(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: CreateLearningCycle,
) -> Result<LearningCycle, AppError> {
    let h = hash(&(id, &input))?;
    let mut receipt = begin(pool, auth, &[]).await?;
    let prior = cycle_receipt(&mut receipt, auth, input.idempotency_key, &h).await?;
    receipt.commit().await.map_err(AppError::internal)?;
    if let Some(result) = prior {
        return cycle_by_id(pool, auth, id, result).await;
    }
    let lists = task_lists(pool, auth, id).await?;
    let mut tx = begin(pool, auth, &lists).await?;
    let t = task(&mut tx, auth, id).await?;
    if let Some(result) = cycle_receipt(&mut tx, auth, input.idempotency_key, &h).await? {
        tx.commit().await.map_err(AppError::internal)?;
        return cycle_by_id(pool, auth, id, result).await;
    }
    let m = meta(&mut tx, id).await?;
    let req = PreviewLearningCycle {
        operation: input.operation,
        after_cycle_id: input.after_cycle_id,
        expected_revision: input.expected_revision,
    };
    let (p, candidates, c) = cycle_preview_in(&mut tx, auth, &t, &m, &req).await?;
    if p.preview.preview_fingerprint != input.preview_fingerprint {
        return Err(conflict("新轮内容或开始日期已变化，请重新核对"));
    }
    if p.preview
        .choice
        .as_ref()
        .is_some_and(|c| c.unavailable_count > 0)
        && !input.accept_partial_choice
    {
        return Err(invalid(
            "部分内容暂不可出单选，请确认仅使用可用内容开启新轮",
        ));
    }
    if !p.preview.can_create {
        return Err(invalid(&p.preview.reasons.join("；")).with_detail_template(
            crate::i18n::errors::DetailTemplate::LearningPlanReasons(p.preview.reasons),
        ));
    }
    let refreshed = if input.operation == CycleOperation::RefreshSources {
        Some(source_refresh::apply(&mut tx, &c, &candidates).await?)
    } else {
        None
    };
    let result = if refreshed.is_some() {
        c.id
    } else {
        if input.operation == CycleOperation::RestartBlocked {
            cancel_cycles(&mut tx, id).await?;
            sqlx::query(
                "UPDATE learning_runs SET state='cancelled' WHERE task_id=$1 AND state='active'",
            )
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        }
        write_cycle(
            &mut tx,
            auth,
            id,
            Some(&c),
            p.earliest_day,
            &p.preview,
            &candidates,
        )
        .await?
    };
    sqlx::query("INSERT INTO learning_cycle_requests(user_id,idempotency_key,request_hash,task_id,after_cycle_id,operation,result_cycle_id) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(auth.subject).bind(input.idempotency_key).bind(h).bind(id).bind(c.id).bind(match input.operation {CycleOperation::Next=>"next",CycleOperation::RestartBlocked=>"restart_blocked",CycleOperation::RefreshSources=>"refresh_sources"}).bind(result).execute(&mut *tx).await.map_err(AppError::internal)?;
    if let Some(ids) = refreshed {
        source_refresh::ensure_sources(&mut tx, auth, &lists, c.id, &ids).await?;
    } else {
        ensure_cycle_sources(&mut tx, auth, &lists, result).await?;
    }
    learner(&mut tx, auth).await?;
    let end = now(&mut tx).await?;
    if business_window(end).0 != p.preview.business_day || t.ends_at.is_some_and(|e| e <= end) {
        return Err(conflict("学习日已变化，请核对新轮计划"));
    }
    tx.commit().await.map_err(AppError::internal)?;
    cycle_by_id(pool, auth, id, result).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn estimates_include_today_remaining_quota() {
        assert_eq!(remaining_days(175, 100, 40), 3);
        assert_eq!(remaining_days(175, 100, 100), 2);
        assert_eq!(remaining_days(175, 100, 0), 2);
        assert_eq!(remaining_days(35, 100, 35), 1);
        assert_eq!(remaining_days(0, 100, 0), 0);
    }
}
