//! Plan views and source availability summaries, using the caller's transaction for cycle summaries.
use super::{CycleRow, Unit, dto::*, latest, meta, remaining_days, schedule, units};
use crate::{
    api::page_meta,
    auth::extract::AuthUser,
    error::AppError,
    learning_tasks::{dto::*, mastery, question::*, repository::*, service},
};
use chrono::Duration;
use sqlx::PgPool;
use uuid::Uuid;

pub(super) async fn summarize(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    t: &LearningTask,
    c: &CycleRow,
    rows: &[Unit],
    current: Option<&RunRow>,
) -> Result<LearningCycle, AppError> {
    let refs: Vec<_> = rows.iter().map(|u| &u.question).collect();
    let access = sources_for_units(tx, auth, &t.wordlist_ids, &refs).await?;
    let mastery = meta(tx, t.id).await?.generation_policy == mastery::MODE;
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
    for u in rows.iter().filter(|u| {
        if mastery {
            u.practice_success_days < 2
        } else {
            u.covered_answer_id.is_none()
        }
    }) {
        if access.can_practice(&u.question)
            || grace.as_ref().is_some_and(|(qs, a)| {
                qs.iter()
                    .any(|q| q.cycle_unit_id == Some(u.question.id) && a.can_practice(q))
            })
        {
            available += 1;
        }
    }
    let covered = rows
        .iter()
        .filter(|u| {
            if mastery {
                u.practice_success_days == 2
            } else {
                u.covered_answer_id.is_some()
            }
        })
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
    let access = sources_for_units(tx, auth, &t.wordlist_ids, &[]).await?;
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
    let c = if m.generation_policy != "legacy_v1" {
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
    let mastery = if m.generation_policy == mastery::MODE {
        Some(mastery::summary(&mut tx, c.as_ref().expect("mastery cycle").id, day).await?)
    } else {
        None
    };
    let (can_start, status) = if mastery.is_some() {
        let c = c.as_ref().expect("mastery cycle");
        let rows = units(&mut tx, c.id).await?;
        let refs: Vec<_> = rows.iter().map(|u| &u.question).collect();
        let access = sources_for_units(&mut tx, auth, &lists, &refs).await?;
        let ids = mastery::eligible(&mut tx, c.id, day).await?;
        let available = ids
            .iter()
            .filter(|id| {
                rows.iter()
                    .any(|u| u.question.id == **id && access.can_practice(&u.question))
            })
            .count() as i64;
        let active = current_run
            .as_ref()
            .is_some_and(|r| r.state == LearningRunState::Active);
        let done = t.task_type == LearningTaskType::Daily
            && current_run
                .as_ref()
                .is_some_and(|r| r.state == LearningRunState::Completed);
        let started = c.earliest_day <= day;
        a = if active {
            current_run
                .as_ref()
                .map_or(0, |r| i64::from(r.target_count) - r.answered_count)
        } else if open && started && !done {
            available
                .min(i64::from(t.daily_question_count.unwrap_or(20)))
                .min(20)
        } else {
            0
        };

        (
            open && started && !done && a > 0,
            if !open {
                if t.state == LearningTaskState::Archived {
                    "archived"
                } else {
                    "schedule_ended"
                }
            } else if !started {
                "not_started"
            } else if done {
                "today_completed"
            } else if a > 0 {
                "ready"
            } else if ids.is_empty() {
                "review_waiting"
            } else {
                "source_blocked"
            },
        )
    } else if let Some(c) = cycle.as_ref() {
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
                    .filter(|q| !ans.contains_key(&q.id) && access.can_practice(q))
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
    let can_next =
        mastery.is_none() && open && cycle.as_ref().is_some_and(|c| c.state == "completed");
    let can_restart = mastery.is_none()
        && open
        && cycle
            .as_ref()
            .is_some_and(|c| c.state == "active" && c.blocked_count > 0);
    let cards = source_cards(&mut tx, auth, &t).await?;
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    let choice = t.question_type.as_deref() == Some(crate::question_bank::learning::CHOICE);
    let mixed = choice
        && !crate::question_bank::learning::is_strict(m.choice_policy.as_deref())
        && (m.choice_policy.as_deref() == Some(crate::question_bank::learning::MIXED_POLICY)
            || c.as_ref()
                .is_some_and(|c| c.generation_version == WORDLIST_GENERATION_VERSION));
    let question_types = if mixed {
        vec!["spelling_zh_to_en".into(), "choice_zh_to_en".into()]
    } else if choice {
        vec!["choice_zh_to_en".into()]
    } else {
        vec!["spelling_zh_to_en".into()]
    };
    Ok(LearningPlan {
        question_types: Some(question_types),
        mastery,
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
    include_mastery: bool,
    include_configured: bool,
) -> Result<LearningPlanPage, AppError> {
    let (p, s) = page(&query)?;
    let mut tx = begin(pool, auth, &[]).await?;
    let state = query.state.unwrap_or(LearningTaskState::Active);
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM learning_tasks WHERE user_id=$1 AND state=$2 AND ($3 OR question_type='spelling_zh_to_en') AND ($4 OR generation_policy<>'mastery_v1') AND ($5 OR choice_policy IS DISTINCT FROM 'mixed_tiered_v1')")
            .bind(auth.subject)
            .bind(state)
            .bind(include_choice)
            .bind(include_mastery)
            .bind(include_configured)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM learning_tasks WHERE user_id=$1 AND state=$2 AND ($5 OR question_type='spelling_zh_to_en') AND ($6 OR generation_policy<>'mastery_v1') AND ($7 OR choice_policy IS DISTINCT FROM 'mixed_tiered_v1') ORDER BY created_at DESC,id DESC LIMIT $3 OFFSET $4").bind(auth.subject).bind(state).bind(i64::from(s)).bind(i64::from(p-1)*i64::from(s)).bind(include_choice).bind(include_mastery).bind(include_configured).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
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

pub(super) async fn cycle_by_id(
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
