//! Coverage plans reuse task/run/answer facts. Main record: tsz/docs/features/task-spelling-creation/.
pub mod dto;
pub mod handler;
use super::{dto::*, question::*, repository::*, service};
use crate::{
    auth::extract::AuthUser,
    error::AppError,
    wordlists::service::{hash, page_meta},
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use dto::*;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct PlanMeta {
    generation_policy: String,
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
    earliest_day: NaiveDate,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}
#[derive(sqlx::FromRow)]
struct Unit {
    #[sqlx(flatten)]
    question: QuestionRow,
    covered_answer_id: Option<Uuid>,
}
async fn meta(tx: &mut Tx<'_>, id: Uuid) -> Result<PlanMeta, AppError> {
    sqlx::query_as("SELECT generation_policy,start_date,end_date FROM learning_tasks WHERE id=$1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)
}
async fn latest(tx: &mut Tx<'_>, id: Uuid) -> Result<CycleRow, AppError> {
    sqlx::query_as("SELECT * FROM learning_cycles WHERE task_id=$1 ORDER BY cycle_number DESC LIMIT 1 FOR UPDATE").bind(id).fetch_one(&mut **tx).await.map_err(AppError::internal)
}
async fn units(tx: &mut Tx<'_>, id: Uuid) -> Result<Vec<Unit>, AppError> {
    sqlx::query_as("SELECT *,id AS cycle_unit_id FROM learning_cycle_units WHERE cycle_id=$1 ORDER BY position").bind(id).fetch_all(&mut **tx).await.map_err(AppError::internal)
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
    let (pool, entry_count, settings) = service::pool_for_type(
        tx,
        auth,
        &input.wordlist_ids,
        input.question_type.as_deref() == Some(crate::question_bank::learning::CHOICE),
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
    let estimated = input
        .daily_question_count
        .map(|n| ceil(count, i64::from(n)));
    let mut reasons = vec![];
    if count == 0 {
        reasons.push(
            if input.question_type.as_deref() == Some(crate::question_bank::learning::CHOICE) {
                "所选词表、级别与英美档位没有已审核可用单选题"
            } else {
                "没有可出题内容，请更换词表；自定义词暂不参与出题"
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
    let preview_fingerprint = digest(&(input, &pool.candidates, &settings, earliest))?;
    learner(tx, auth).await?;
    Ok((
        LearningPlanPreview {
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
    let choice = candidates
        .first()
        .is_some_and(|c| c.answer.choice.is_some());
    sqlx::query("INSERT INTO learning_cycles(id,task_id,user_id,cycle_number,state,total_count,settings_snapshot,generation_version,grading_version,after_cycle_id,earliest_day) VALUES($1,$2,$3,$4,'active',$5,$6,$7,$8,$9,$10)").bind(id).bind(task_id).bind(auth.subject).bind(predecessor.map_or(1, |c| c.cycle_number+1)).bind(candidates.len() as i32).bind(serde_json::to_value(&preview.settings).map_err(AppError::internal)?).bind(if choice {"choice_bank_v1"} else {GENERATION_VERSION}).bind(if choice {crate::question_bank::learning::GRADING} else {GRADING_VERSION}).bind(predecessor.map(|c|c.id)).bind(earliest).execute(&mut **tx).await.map_err(AppError::internal)?;
    for (position, c) in candidates.iter().enumerate() {
        let mut answer = c.answer.clone();
        crate::question_bank::learning::shuffle_answer(&mut answer);
        sqlx::query("INSERT INTO learning_cycle_units(id,cycle_id,position,source_wordlist_id,source_revision,source_membership_id,source_public_generation,entry_id,entry_archive_generation,publication_id,pos_id,sense_id,definition_id,form_ids,unit_key,prompt_snapshot,answer_snapshot,fingerprint) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)")
   .bind(Uuid::now_v7()).bind(id).bind(position as i32).bind(c.source_wordlist_id).bind(c.source_revision).bind(c.source_membership_id).bind(c.source_public_generation).bind(c.entry_id).bind(c.entry_archive_generation).bind(c.publication_id).bind(c.pos_id).bind(c.sense_id).bind(c.definition_id).bind(&c.form_ids).bind(&c.unit_key).bind(serde_json::to_value(&c.prompt).map_err(AppError::internal)?).bind(serde_json::to_value(&answer).map_err(AppError::internal)?).bind(hash(c)?).execute(&mut **tx).await.map_err(AppError::internal)?;
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
    let mut tx = begin(pool, auth, &input.wordlist_ids).await?;
    if let Some(id) = service::create_receipt(&mut tx, auth, input.idempotency_key, &h).await? {
        tx.commit().await.map_err(AppError::internal)?;
        return detail(pool, auth, id).await;
    }
    let (p, candidates) = preview_in(&mut tx, auth, &input.preview(), None).await?;
    if p.preview_fingerprint != input.preview_fingerprint {
        return Err(conflict("题量、来源或学习设置已变化，请重新核对预览后创建"));
    }
    if !p.can_create {
        return Err(invalid(&p.reasons.join("；")));
    }
    let id = Uuid::now_v7();
    let s = input.schedule.as_ref();
    sqlx::query("INSERT INTO learning_tasks(id,user_id,name,task_type,wordlist_ids,daily_question_count,ends_at,create_key,create_hash,generation_policy,start_date,end_date,question_type) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'coverage_v1',$10,$11,$12)")
 .bind(id).bind(auth.subject).bind(input.name.trim()).bind(input.task_type).bind(&input.wordlist_ids).bind(input.daily_question_count).bind(s.map(|s|boundary(s.end_date+Duration::days(1)))).bind(input.idempotency_key).bind(h).bind(s.map(|s|s.start_date)).bind(s.map(|s|s.end_date)).bind(input.question_type.as_deref().unwrap_or("spelling_zh_to_en")).execute(&mut *tx).await.map_err(AppError::internal)?;
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
    let entries: Vec<_> = rows.iter().map(|u| u.question.entry_id).collect();
    let access = super::repository::sources_for_units(
        tx,
        auth,
        lists,
        &entries,
        rows.iter()
            .any(|u| u.question.answer_snapshot.get("choice").is_some()),
    )
    .await?;
    if rows.iter().any(|u| !access.available(&u.question)) {
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
    let has:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM learning_tasks WHERE user_id=$1 AND ($2::uuid IS NULL OR id=$2) AND generation_policy='coverage_v1' AND ($2::uuid IS NOT NULL OR question_type='spelling_zh_to_en'))").bind(auth.subject).bind(id).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
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
        let cycle:Uuid=sqlx::query_scalar("UPDATE learning_cycle_units SET covered_answer_id=$2 WHERE id=$1 AND covered_answer_id IS NULL RETURNING cycle_id").bind(unit).bind(answer).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
        sqlx::query("UPDATE learning_cycles SET state='completed',completed_at=clock_timestamp() WHERE id=$1 AND state='active' AND NOT EXISTS(SELECT 1 FROM learning_cycle_units WHERE cycle_id=$1 AND covered_answer_id IS NULL)").bind(cycle).execute(&mut **tx).await.map_err(AppError::internal)?;
    }
    Ok(())
}
async fn summarize(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    t: &LearningTask,
    c: &CycleRow,
    rows: &[Unit],
    current: Option<&RunRow>,
) -> Result<LearningCycle, AppError> {
    let fixed: Vec<_> = rows.iter().map(|u| u.question.entry_id).collect();
    let access = super::repository::sources_for_units(
        tx,
        auth,
        &t.wordlist_ids,
        &fixed,
        t.question_type.is_some(),
    )
    .await?;
    let mut available = 0;
    // Only an active, unexpired run may lend its deletion grace to its own units.
    let time = now(tx).await?;
    let grace = if let Some(r) = current.filter(|r| r.state == LearningRunState::Active) {
        if r.expires_at.is_none_or(|e| e > time) {
            let qs = questions(tx, r.id).await?;
            let a = sources_for_questions(tx, auth, &t.wordlist_ids, r, &qs).await?;
            Some((qs, a))
        } else {
            None
        }
    } else {
        None
    };
    for u in rows.iter().filter(|u| u.covered_answer_id.is_none()) {
        if access.available(&u.question)
            || grace.as_ref().is_some_and(|(qs, a)| {
                qs.iter()
                    .any(|q| q.cycle_unit_id == Some(u.question.id) && a.available(q))
            })
        {
            available += 1;
        }
    }
    let covered = rows
        .iter()
        .filter(|u| u.covered_answer_id.is_some())
        .count() as i64;
    let remaining = i64::from(c.total_count) - covered;
    Ok(LearningCycle {
        id: c.id,
        cycle_number: c.cycle_number,
        state: c.state.clone(),
        total_count: c.total_count,
        covered_count: covered,
        remaining_count: remaining,
        available_remaining_count: available,
        blocked_count: remaining - available,
        earliest_day: c.earliest_day,
        created_at: c.created_at,
        completed_at: c.completed_at,
    })
}
async fn source_cards(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    t: &LearningTask,
) -> Result<Vec<LearningPlanSource>, AppError> {
    let access =
        super::repository::sources_for_units(tx, auth, &t.wordlist_ids, &[], false).await?;
    let mut cards = vec![];
    for id in &t.wordlist_ids {
        let visible = access.lists.contains_key(id);
        let row: Option<(String, Option<String>, String, i64)> = if visible {
            sqlx::query_as("SELECT w.name,COALESCE(u.display_name,a.display_name),w.state,(SELECT count(*) FROM wordlist_items WHERE wordlist_id=w.id) FROM wordlists w LEFT JOIN users u ON u.id=w.owner_user_id LEFT JOIN admins a ON a.id=w.owner_admin_id WHERE w.id=$1").bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?
        } else {
            None
        };
        let note: Option<String> = if visible {
            sqlx::query_scalar("SELECT name FROM wordlist_library_nodes WHERE user_id=$1 AND wordlist_id=$2 AND trashed_at IS NULL ORDER BY space LIMIT 1").bind(auth.subject).bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?
        } else {
            None
        };
        cards.push(LearningPlanSource {
            id: *id,
            available: visible,
            name: row.as_ref().map(|r| r.0.clone()),
            personal_note: note,
            author_name: row
                .as_ref()
                .and_then(|r| r.1.clone())
                .or_else(|| visible.then(|| "平台".into())),
            state: row.as_ref().map(|r| r.2.clone()),
            item_count: row.map(|r| r.3),
        });
    }
    Ok(cards)
}
pub async fn detail(pool: &PgPool, auth: &AuthUser, id: Uuid) -> Result<LearningPlan, AppError> {
    let lists = task_lists(pool, auth, id).await?;
    let mut tx = begin(pool, auth, &lists).await?;
    let t = task(&mut tx, auth, id).await?;
    let m = meta(&mut tx, id).await?;
    let at = now(&mut tx).await?;
    let (day, _, next_day_at) = business_window(at);
    let c = if m.generation_policy == "coverage_v1" {
        Some(latest(&mut tx, id).await?)
    } else {
        None
    };
    let run_id:Option<Uuid>=sqlx::query_scalar("SELECT id FROM learning_runs WHERE task_id=$1 AND ($2='longterm' OR business_day=$3) AND ($4::uuid IS NULL OR cycle_id=$4) ORDER BY started_at DESC,id DESC LIMIT 1").bind(id).bind(t.task_type).bind(day).bind(c.as_ref().map(|c|c.id)).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    let rr = if let Some(id) = run_id {
        Some(run(&mut tx, auth, id).await?)
    } else {
        None
    };
    let current_run = if let Some(r) = rr.as_ref() {
        let qs = questions(&mut tx, r.id).await?;
        let a = sources_for_questions(&mut tx, auth, &lists, r, &qs).await?;
        Some(run_view(&mut tx, r, &qs, &a).await?)
    } else {
        None
    };
    let cycle = if let Some(c) = c.as_ref() {
        let rows = units(&mut tx, c.id).await?;
        Some(summarize(&mut tx, auth, &t, c, &rows, rr.as_ref()).await?)
    } else {
        None
    };
    let open = t.state == LearningTaskState::Active && t.ends_at.is_none_or(|e| e > at);
    let mut a = 0;
    let mut estimated = None;
    let mut from = None;
    let mut risk = false;
    let mut reasons = vec![];
    let (can_start, status) = if let Some(c) = cycle.as_ref() {
        let started = c.earliest_day <= day;
        let active = c.state == "active";
        let done = current_run
            .as_ref()
            .is_some_and(|r| r.state == LearningRunState::Completed);
        if open && started && active && !done {
            if let Some(r) = current_run
                .as_ref()
                .filter(|r| r.state == LearningRunState::Active)
            {
                let qs = questions(&mut tx, r.id).await?;
                let ans = answers(&mut tx, r.id).await?;
                let row = rr.as_ref().expect("run row");
                let access = sources_for_questions(&mut tx, auth, &lists, row, &qs).await?;
                a = qs
                    .iter()
                    .filter(|q| !ans.contains_key(&q.id) && access.available(q))
                    .count() as i64;
            } else {
                a = c
                    .available_remaining_count
                    .min(i64::from(t.daily_question_count.unwrap_or(c.total_count)));
            }
        }
        if let Some(n) = t.daily_question_count {
            if c.remaining_count == 0 {
                estimated = Some(0);
                from = Some(day);
            } else if open && c.blocked_count == 0 {
                estimated = Some(remaining_days(c.remaining_count, i64::from(n), a));
                from = Some(if a > 0 {
                    day
                } else {
                    (day + Duration::days(1)).max(c.earliest_day)
                });
            }
            if let Some(end) = m.end_date {
                let future = (end - day.max(c.earliest_day - Duration::days(1)))
                    .num_days()
                    .max(0);
                risk = a + future * i64::from(n) < c.remaining_count;
            }
        }
        if c.blocked_count > 0 {
            reasons.push(format!(
                "有 {} 题待恢复，暂无法预计整轮完成时间",
                c.blocked_count
            ));
        }
        (
            a > 0,
            if t.state == LearningTaskState::Archived {
                "archived"
            } else if !open {
                "schedule_ended"
            } else if c.state == "completed" {
                "cycle_completed"
            } else if !started {
                "not_started"
            } else if done {
                "today_completed"
            } else if a == 0 {
                "source_blocked"
            } else {
                "ready"
            },
        )
    } else {
        let can = open
            && current_run
                .as_ref()
                .is_none_or(|r| r.state == LearningRunState::Active);
        (
            can,
            if !open {
                "archived"
            } else if can {
                "ready"
            } else {
                "today_completed"
            },
        )
    };
    let can_next = open && cycle.as_ref().is_some_and(|c| c.state == "completed");
    let can_restart = open
        && cycle
            .as_ref()
            .is_some_and(|c| c.state == "active" && c.blocked_count > 0);
    let cards = source_cards(&mut tx, auth, &t).await?;
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(LearningPlan {
        task: t,
        mode: m.generation_policy.clone(),
        schedule: schedule(&m),
        sources: cards,
        current_cycle: cycle,
        current_run,
        today_available_count: a,
        estimated_learning_days: estimated,
        estimate_from_day: from,
        schedule_at_risk: risk,
        status: status.into(),
        can_start,
        can_start_next_cycle: can_next,
        can_restart_blocked_cycle: can_restart,
        reasons,
        server_time: at,
        business_day: day,
        next_day_at,
        timezone: "Asia/Shanghai".into(),
    })
}
pub async fn list(
    pool: &PgPool,
    auth: &AuthUser,
    query: LearningPageQuery,
    include_choice: bool,
) -> Result<LearningPlanPage, AppError> {
    let (p, s) = page(&query)?;
    let mut tx = begin(pool, auth, &[]).await?;
    let state = query.state.unwrap_or(LearningTaskState::Active);
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM learning_tasks WHERE user_id=$1 AND state=$2 AND ($3 OR question_type='spelling_zh_to_en')")
            .bind(auth.subject)
            .bind(state)
            .bind(include_choice)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM learning_tasks WHERE user_id=$1 AND state=$2 AND ($5 OR question_type='spelling_zh_to_en') ORDER BY created_at DESC,id DESC LIMIT $3 OFFSET $4").bind(auth.subject).bind(state).bind(i64::from(s)).bind(i64::from(p-1)*i64::from(s)).bind(include_choice).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    let mut items = vec![];
    for id in ids {
        items.push(detail(pool, auth, id).await?);
    }
    Ok(LearningPlanPage {
        items,
        pagination: page_meta(p, s, total),
    })
}
pub async fn rename(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    input: RenameLearningTask,
) -> Result<LearningPlan, AppError> {
    let mut tx = begin(pool, auth, &[]).await?;
    task(&mut tx, auth, id).await?;
    if meta(&mut tx, id).await?.generation_policy == "coverage_v1" {
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
        let row = run(&mut tx, auth, last).await?;
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
        let entries: Vec<_> = rows.iter().map(|u| u.question.entry_id).collect();
        let access = super::repository::sources_for_units(
            &mut tx,
            auth,
            &lists,
            &entries,
            t.question_type.is_some(),
        )
        .await?;
        let pending: Vec<_> = rows
            .iter()
            .filter(|u| u.covered_answer_id.is_none() && access.available(&u.question))
            .collect();
        if pending.is_empty() {
            return Err(conflict("来源不可用，已保留本轮进度，请检查来源"));
        }
        let daily = t.task_type == LearningTaskType::Daily;
        let selected: Vec<Uuid> = if daily {
            pending
                .iter()
                .take(t.daily_question_count.expect("daily count") as usize)
                .map(|u| u.question.id)
                .collect()
        } else {
            rows.iter().map(|u| u.question.id).collect()
        };
        let run_id = Uuid::now_v7();
        let expires = daily.then(|| t.ends_at.map_or(window_end, |e| e.min(window_end)));
        sqlx::query("INSERT INTO learning_runs(id,task_id,user_id,task_revision,task_type,business_day,window_start,window_end,expires_at,settings_snapshot,seed,target_count,state,generation_version,grading_version,cycle_id,question_type) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'active',$13,$14,$15,$16)")
  .bind(run_id).bind(id).bind(auth.subject).bind(t.revision).bind(t.task_type).bind(daily.then_some(day)).bind(daily.then_some(window_start)).bind(daily.then_some(window_end)).bind(expires).bind(&c.settings_snapshot).bind(Uuid::now_v7()).bind(selected.len() as i32).bind(if t.question_type.is_some() {"choice_bank_v1"} else {GENERATION_VERSION}).bind(if t.question_type.as_deref()==Some(crate::question_bank::learning::CHOICE) {crate::question_bank::learning::GRADING} else {GRADING_VERSION}).bind(c.id).bind(t.question_type.as_deref().unwrap_or("spelling_zh_to_en")).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("INSERT INTO learning_questions(id,run_id,cycle_id,cycle_unit_id,position,source_wordlist_id,source_revision,source_membership_id,source_public_generation,entry_id,entry_archive_generation,publication_id,pos_id,sense_id,definition_id,form_ids,unit_key,prompt_snapshot,answer_snapshot,fingerprint) SELECT gen_random_uuid(),$1,cycle_id,id,(row_number() OVER(ORDER BY position)-1)::integer,source_wordlist_id,source_revision,source_membership_id,source_public_generation,entry_id,entry_archive_generation,publication_id,pos_id,sense_id,definition_id,form_ids,unit_key,prompt_snapshot,answer_snapshot,fingerprint FROM learning_cycle_units WHERE id=ANY($2) ORDER BY position").bind(run_id).bind(&selected).execute(&mut *tx).await.map_err(AppError::internal)?;
        run_id
    };
    let row = run(&mut tx, auth, run_id).await?;
    let qs = questions(&mut tx, run_id).await?;
    let access = sources_for_questions(&mut tx, auth, &lists, &row, &qs).await?;
    let ans = answers(&mut tx, run_id).await?;
    if row.state == LearningRunState::Active
        && !qs
            .iter()
            .any(|q| !ans.contains_key(&q.id) && access.available(q))
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
async fn cycle_preview_in(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    t: &LearningTask,
    m: &PlanMeta,
    input: &PreviewLearningCycle,
) -> Result<(LearningCyclePreview, Vec<Candidate>, CycleRow), AppError> {
    if m.generation_policy != "coverage_v1" {
        return Err(conflict("旧模式任务不支持覆盖轮次，请新建任务"));
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
async fn cycle_by_id(
    pool: &PgPool,
    auth: &AuthUser,
    task_id: Uuid,
    id: Uuid,
) -> Result<LearningCycle, AppError> {
    let lists = task_lists(pool, auth, task_id).await?;
    let mut tx = begin(pool, auth, &lists).await?;
    let t = task(&mut tx, auth, task_id).await?;
    let c: CycleRow =
        sqlx::query_as("SELECT * FROM learning_cycles WHERE id=$1 AND task_id=$2 FOR UPDATE")
            .bind(id)
            .bind(task_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let rows = units(&mut tx, id).await?;
    let result = summarize(&mut tx, auth, &t, &c, &rows, None).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
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
    if !p.preview.can_create {
        return Err(invalid(&p.preview.reasons.join("；")));
    }
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
    let result = write_cycle(
        &mut tx,
        auth,
        id,
        Some(&c),
        p.earliest_day,
        &p.preview,
        &candidates,
    )
    .await?;
    sqlx::query("INSERT INTO learning_cycle_requests(user_id,idempotency_key,request_hash,task_id,after_cycle_id,operation,result_cycle_id) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(auth.subject).bind(input.idempotency_key).bind(h).bind(id).bind(c.id).bind(if input.operation==CycleOperation::Next{"next"}else{"restart_blocked"}).bind(result).execute(&mut *tx).await.map_err(AppError::internal)?;
    ensure_cycle_sources(&mut tx, auth, &lists, result).await?;
    learner(&mut tx, auth).await?;
    let end = now(&mut tx).await?;
    if business_window(end).0 != p.preview.business_day || t.ends_at.is_some_and(|e| e <= end) {
        return Err(conflict("学习日已变化，请核对新轮计划"));
    }
    tx.commit().await.map_err(AppError::internal)?;
    cycle_by_id(pool, auth, id, result).await
}
pub async fn cycles(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    query: LearningPageQuery,
) -> Result<LearningCyclePage, AppError> {
    let (p, s) = page(&query)?;
    let mut tx = begin(pool, auth, &[]).await?;
    task(&mut tx, auth, id).await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_cycles WHERE task_id=$1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM learning_cycles WHERE task_id=$1 ORDER BY cycle_number DESC LIMIT $2 OFFSET $3").bind(id).bind(i64::from(s)).bind(i64::from(p-1)*i64::from(s)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    let mut items = vec![];
    for cid in ids {
        items.push(cycle_by_id(pool, auth, id, cid).await?);
    }
    Ok(LearningCyclePage {
        items,
        pagination: page_meta(p, s, total),
    })
}
pub async fn history(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    query: LearningPageQuery,
) -> Result<LearningPlanRunPage, AppError> {
    let history = service::history(pool, auth, id, query).await?;
    let mut items = vec![];
    for r in history.items {
        let cycle_id: Option<Uuid> =
            sqlx::query_scalar("SELECT cycle_id FROM learning_runs WHERE id=$1 AND user_id=$2")
                .bind(r.id)
                .bind(auth.subject)
                .fetch_one(pool)
                .await
                .map_err(AppError::internal)?;
        items.push(LearningPlanRun { run: r, cycle_id });
    }
    Ok(LearningPlanRunPage {
        items,
        pagination: history.pagination,
    })
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
