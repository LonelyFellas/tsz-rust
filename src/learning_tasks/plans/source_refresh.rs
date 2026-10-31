//! Explicitly rebind current, authorized sources while retaining immutable answer facts.
use super::*;
use std::collections::{HashMap, HashSet};

// Custom item identity is scoped to its wordlist; platform units use entry/POS/sense.
type Identity = (
    Option<Uuid>,
    Option<Uuid>,
    Option<Uuid>,
    Option<(Uuid, Uuid)>,
);
fn identity(q: &QuestionRow) -> Identity {
    (
        q.entry_id,
        q.pos_id,
        q.sense_id,
        q.custom_item_id.map(|item| (q.source_wordlist_id, item)),
    )
}
fn candidate_identity(c: &Candidate) -> Identity {
    (
        c.entry_id,
        c.pos_id,
        c.sense_id,
        c.custom_item_id.map(|item| (c.source_wordlist_id, item)),
    )
}
fn same_content(unit: &Unit, candidate: &Candidate) -> Result<bool, AppError> {
    let old: AnswerSnapshot = serde_json::from_value(unit.question.answer_snapshot.clone())
        .map_err(AppError::internal)?;
    let old_keys: HashSet<_> = old.keys.iter().collect();
    let new_keys: HashSet<_> = candidate.answer.keys.iter().collect();
    Ok(unit.question.prompt_snapshot
        == serde_json::to_value(&candidate.prompt).map_err(AppError::internal)?
        && old.normalization_version == candidate.answer.normalization_version
        && old.choice.is_some() == candidate.answer.choice.is_some()
        && old_keys == new_keys)
}
pub(super) async fn preview_in(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    t: &LearningTask,
    m: &PlanMeta,
    input: &PreviewLearningCycle,
) -> Result<(LearningCyclePreview, Vec<Candidate>, CycleRow), AppError> {
    if m.generation_policy != super::super::mastery::MODE {
        return Err(conflict("此任务不支持保留掌握进度的来源更新"));
    }
    let c = latest(tx, t.id).await?;
    if c.id != input.after_cycle_id || t.revision != input.expected_revision {
        return Err(conflict("任务或学习内容已变化，请刷新后重新确认"));
    }
    let at = now(tx).await?;
    let day = business_window(at).0;
    if t.state != LearningTaskState::Active
        || c.state != "active"
        || t.ends_at.is_some_and(|end| end <= at)
    {
        return Err(unavailable());
    }
    let rows = units(tx, c.id).await?;
    let old = summarize(tx, auth, t, &c, &rows, None).await?;
    let by_identity: HashMap<_, _> = rows.iter().map(|u| (identity(&u.question), u)).collect();
    let mut entries: Vec<_> = rows.iter().filter_map(|u| u.question.entry_id).collect();
    entries.sort();
    entries.dedup();
    // Current access only: an in-flight deletion grace must not enroll new sources.
    let access = sources_for_entries(tx, auth, &t.wordlist_ids, &entries).await?;
    let settings: LearningContext =
        serde_json::from_value(c.settings_snapshot.clone()).map_err(AppError::internal)?;
    let pool = service::candidates_for_sources(
        tx,
        &access,
        &settings,
        t.question_type.as_deref() == Some(crate::question_bank::learning::CHOICE),
        true,
        m.choice_policy.is_some(),
    )
    .await?;
    let candidates: Vec<_> = pool
        .candidates
        .into_iter()
        .filter(|candidate| by_identity.contains_key(&candidate_identity(candidate)))
        .collect();
    let mut changed = 0;
    for candidate in &candidates {
        if !same_content(by_identity[&candidate_identity(candidate)], candidate)? {
            changed += 1;
        }
    }
    // Include both old scheduling state and new sources so an answer, source change or
    // competing refresh between preview and confirmation invalidates the preview.
    let prior: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(u) ORDER BY position) FROM learning_cycle_units u WHERE cycle_id=$1",
    ).bind(c.id).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
    let fingerprint = digest(&(input, t.id, &prior, &candidates, &settings, day))?;
    let count = candidates.len() as i64;
    let reasons = if count == 0 {
        vec!["当前没有可重新确认的词义，请先恢复来源或补足可靠干扰项".into()]
    } else {
        vec![]
    };
    Ok((
        LearningCyclePreview {
            source_refresh: Some(SourceRefreshPreview {
                refreshed_count: count,
                changed_content_count: changed,
                unavailable_count: rows.len() as i64 - count,
            }),
            preview: LearningPlanPreview {
                choice: None,
                entry_count: candidates
                    .iter()
                    .filter_map(|c| c.entry_id)
                    .collect::<HashSet<_>>()
                    .len() as i64,
                eligible_count: count,
                custom_word_count: candidates
                    .iter()
                    .filter(|c| c.custom_item_id.is_some())
                    .count() as i64,
                exclusions: pool.exclusions,
                settings,
                estimated_learning_days: None,
                available_learning_days: None,
                can_create: count > 0,
                reasons,
                preview_fingerprint: fingerprint,
                server_time: at,
                business_day: day,
                timezone: "Asia/Shanghai".into(),
            },
            earliest_day: c.earliest_day.max(day),
            previous_covered_count: old.covered_count,
            previous_total_count: old.total_count,
            previous_blocked_count: old.blocked_count,
        },
        candidates,
        c,
    ))
}
pub(super) async fn apply(
    tx: &mut Tx<'_>,
    c: &CycleRow,
    candidates: &[Candidate],
) -> Result<Vec<Uuid>, AppError> {
    let rows = units(tx, c.id).await?;
    let by_identity: HashMap<_, _> = rows.iter().map(|u| (identity(&u.question), u)).collect();
    let day = business_window(now(tx).await?).0;
    let mut refreshed = Vec::new();
    for candidate in candidates {
        let unit = by_identity[&candidate_identity(candidate)];
        let keep = same_content(unit, candidate)?;
        let mut answer = candidate.answer.clone();
        if let Some(choice) = answer.choice.as_mut() {
            choice.deferred = true;
            choice.options.clear();
            choice.correct_option_id.clear();
            choice.content.options.clear();
            choice.content.correct_option_id.clear();
            choice.content.explanation.clear();
        }
        sqlx::query("UPDATE learning_cycle_units SET source_wordlist_id=$2,source_revision=$3,source_membership_id=$4,source_public_generation=$5,entry_archive_generation=$6,publication_id=$7,definition_id=$8,form_ids=$9,prompt_snapshot=$10,answer_snapshot=$11,fingerprint=$12,practice_success_days=CASE WHEN $13 THEN practice_success_days ELSE 0 END,practice_last_success_day=CASE WHEN $13 THEN practice_last_success_day ELSE NULL END,practice_interval_step=CASE WHEN $13 THEN practice_interval_step ELSE 0 END,practice_last_day=CASE WHEN $13 THEN practice_last_day WHEN covered_answer_id IS NULL THEN NULL ELSE $14::date-1 END,practice_next_day=CASE WHEN $13 THEN practice_next_day WHEN covered_answer_id IS NULL THEN NULL ELSE $14::date END WHERE id=$1")
            .bind(unit.question.id).bind(candidate.source_wordlist_id).bind(candidate.source_revision)
            .bind(candidate.source_membership_id).bind(candidate.source_public_generation)
            .bind(candidate.entry_archive_generation).bind(candidate.publication_id).bind(candidate.definition_id)
            .bind(&candidate.form_ids).bind(serde_json::to_value(&candidate.prompt).map_err(AppError::internal)?)
            .bind(serde_json::to_value(answer).map_err(AppError::internal)?).bind(hash(candidate)?)
            .bind(keep).bind(day).execute(&mut **tx).await.map_err(AppError::internal)?;
        refreshed.push(unit.question.id);
    }
    // Refreshed units use the new projection; keep old runs/answers on their original versions.
    sqlx::query("UPDATE learning_cycles c SET generation_version=$2,grading_version=CASE WHEN t.question_type='choice_zh_to_en' THEN $3 ELSE $4 END FROM learning_tasks t WHERE c.id=$1 AND t.id=c.task_id")
        .bind(c.id).bind(WORDLIST_GENERATION_VERSION).bind(WORDLIST_GRADING_VERSION).bind(GRADING_VERSION)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    // Existing questions/answers remain untouched; their original source and grading
    // snapshot must never be rewritten by a renewed access grant.
    sqlx::query("UPDATE learning_runs SET state='cancelled' WHERE cycle_id=$1 AND state='active'")
        .bind(c.id)
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    Ok(refreshed)
}
pub(super) async fn ensure_sources(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    lists: &[Uuid],
    cycle: Uuid,
    ids: &[Uuid],
) -> Result<(), AppError> {
    let rows = units(tx, cycle).await?;
    let ids: HashSet<_> = ids.iter().copied().collect();
    let refs: Vec<_> = rows
        .iter()
        .filter(|u| ids.contains(&u.question.id))
        .map(|u| &u.question)
        .collect();
    let access = sources_for_units(tx, auth, lists, &refs).await?;
    if refs.len() != ids.len() || refs.iter().any(|q| !access.available(q)) {
        return Err(unavailable());
    }
    Ok(())
}
