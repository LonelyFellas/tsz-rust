use super::{dto::*, queries, *};
use crate::{
    auth::extract::AuthUser,
    learning_tasks::{
        question::{business_window, grade_spelling},
        repository::{conflict, invalid, now},
    },
    state::AppState,
};
use chrono::Duration;
use sqlx::PgPool;

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: Uuid,
    settings_snapshot: serde_json::Value,
    expires_at: DateTime<Utc>,
}
#[derive(sqlx::FromRow)]
struct ItemRow {
    id: Uuid,
    memory_id: Uuid,
    memory_revision: i64,
    memory_epoch: i64,
    content_signature: Vec<u8>,
    source_wordlist_id: Uuid,
    source_membership_id: Uuid,
    source_public_generation: Option<i64>,
    entry_id: Uuid,
    entry_archive_generation: i64,
    pos_id: Uuid,
    sense_id: Uuid,
    prompt_snapshot: serde_json::Value,
    answer_snapshot: serde_json::Value,
    grading_version: String,
    kind: String,
    expires_at: DateTime<Utc>,
    revealed_at: Option<DateTime<Utc>>,
    active: bool,
}
impl ItemRow {
    fn question(&self) -> QuestionRow {
        QuestionRow {
            cycle_unit_id: None,
            id: self.id,
            position: 0,
            source_wordlist_id: self.source_wordlist_id,
            source_membership_id: self.source_membership_id,
            source_public_generation: self.source_public_generation,
            entry_id: self.entry_id,
            entry_archive_generation: self.entry_archive_generation,
            pos_id: self.pos_id,
            sense_id: self.sense_id,
            prompt_snapshot: self.prompt_snapshot.clone(),
            answer_snapshot: self.answer_snapshot.clone(),
        }
    }
}
#[derive(sqlx::FromRow)]
struct AttemptRow {
    id: Uuid,
    item_id: Uuid,
    submitted_answer: String,
    is_correct: bool,
    effect: String,
    accepted_at: DateTime<Utc>,
}
async fn session(tx: &mut Tx<'_>, user: Uuid, id: Uuid) -> Result<SessionRow, AppError> {
    sqlx::query_as("SELECT id,settings_snapshot,expires_at FROM learning_review_sessions WHERE id=$1 AND user_id=$2")
        .bind(id).bind(user).fetch_optional(&mut **tx).await.map_err(AppError::internal)?.ok_or_else(||AppError::not_found("复习不存在"))
}
async fn rows(tx: &mut Tx<'_>, id: Uuid) -> Result<Vec<ItemRow>, AppError> {
    sqlx::query_as("SELECT * FROM learning_review_items WHERE session_id=$1 ORDER BY position")
        .bind(id)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)
}
async fn access(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    items: &[ItemRow],
) -> Result<SourceAccess, AppError> {
    let mut lists: Vec<_> = items.iter().map(|i| i.source_wordlist_id).collect();
    lists.sort();
    lists.dedup();
    let entries: Vec<_> = items.iter().map(|i| i.entry_id).collect();
    crate::learning_tasks::repository::sources_for_entries(tx, auth, &lists, &entries).await
}
async fn view_in(tx: &mut Tx<'_>, auth: &AuthUser, id: Uuid) -> Result<ReviewSession, AppError> {
    let s = session(tx, auth.subject, id).await?;
    let items = rows(tx, id).await?;
    let visible = access(tx, auth, &items).await?;
    let at = now(tx).await?;
    sqlx::query("UPDATE learning_review_items i SET active=false FROM learning_memories m WHERE i.session_id=$1 AND i.memory_id=m.id AND i.active AND (m.paused OR (i.kind='retest' AND (m.revision<>i.memory_revision OR m.memory_epoch<>i.memory_epoch)))")
        .bind(id).execute(&mut **tx).await.map_err(AppError::internal)?;
    let inactive: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM learning_review_items WHERE session_id=$1 AND NOT active",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    let attempts: Vec<AttemptRow> =
        sqlx::query_as("SELECT * FROM learning_review_attempts WHERE session_id=$1")
            .bind(id)
            .fetch_all(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    let answered_count = attempts.len() as i64;
    let target_count = items.len() as i64;
    let mut output = Vec::new();
    for i in items {
        let available = visible.available(&i.question());
        let a = attempts.iter().find(|a| a.item_id == i.id);
        let answers: AnswerSnapshot =
            serde_json::from_value(i.answer_snapshot.clone()).map_err(AppError::internal)?;
        let feedback = if available {
            a.map(|a| ReviewFeedback {
                answer_id: a.id,
                is_correct: a.is_correct,
                submitted_answer: a.submitted_answer.clone(),
                accepted_answers: answers.answers.clone(),
                memory_effect: a.effect.clone(),
                accepted_at: a.accepted_at,
            })
        } else {
            None
        };
        output.push(ReviewItem {
            id: i.id,
            memory_id: i.memory_id,
            kind: i.kind,
            answered: a.is_some(),
            content_available: available
                && (a.is_some() || (i.active && !inactive.contains(&i.id) && i.expires_at > at)),
            revealed: i.revealed_at.is_some(),
            prompt: if available {
                Some(serde_json::from_value(i.prompt_snapshot).map_err(AppError::internal)?)
            } else {
                None
            },
            revealed_answers: if available && i.revealed_at.is_some() {
                Some(answers.answers)
            } else {
                None
            },
            feedback,
            expires_at: i.expires_at,
        });
    }
    let state = if answered_count == target_count {
        "completed"
    } else if s.expires_at <= at {
        "expired"
    } else if output.iter().all(|i| i.answered || !i.content_available) {
        "blocked"
    } else {
        "active"
    };
    Ok(ReviewSession {
        id: s.id,
        state: state.into(),
        settings: serde_json::from_value(s.settings_snapshot).map_err(AppError::internal)?,
        items: output,
        target_count,
        answered_count,
        server_time: at,
        expires_at: s.expires_at,
    })
}
pub(super) async fn get(
    state: &AppState,
    auth: &AuthUser,
    id: Uuid,
) -> Result<ReviewSession, AppError> {
    queries::require_feature(state)?;
    let (mut tx, _) = queries::begin(&state.pool, auth).await?;
    let result = view_in(&mut tx, auth, id).await?;
    crate::learning_tasks::repository::learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
async fn save_start(
    tx: &mut Tx<'_>,
    user: Uuid,
    input: &StartReview,
    h: &[u8],
    session: Uuid,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO learning_review_start_requests(user_id,request_key,request_hash,session_id) VALUES($1,$2,$3,$4)")
        .bind(user).bind(input.idempotency_key).bind(h).bind(session).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
pub(super) async fn start(
    state: &AppState,
    auth: &AuthUser,
    input: StartReview,
) -> Result<ReviewSession, AppError> {
    queries::require_feature(state)?;
    let limit = input.limit.unwrap_or(20);
    if !(1..=20).contains(&limit) {
        return Err(invalid("每批请选择1–20项"));
    }
    let h = hash(&input)?;
    let (mut tx, context) = queries::begin(&state.pool, auth).await?;
    if !queries::enabled(&mut tx, auth.subject).await? {
        return Err(conflict("请先开启个人记忆"));
    }
    let prior:Option<(Vec<u8>,Uuid)>=sqlx::query_as("SELECT request_hash,session_id FROM learning_review_start_requests WHERE user_id=$1 AND request_key=$2")
        .bind(auth.subject).bind(input.idempotency_key).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    if let Some((old, id)) = prior {
        if old != h {
            return Err(conflict("请求键已用于其他复习"));
        }
        tx.commit().await.map_err(AppError::internal)?;
        return get(state, auth, id).await;
    }
    let at = now(&mut tx).await?;
    let (day, _, day_end) = business_window(at);
    sqlx::query("UPDATE learning_review_items i SET active=false WHERE i.user_id=$1 AND i.active AND (i.expires_at<=$2 OR EXISTS(SELECT 1 FROM learning_memories m WHERE m.id=i.memory_id AND (m.paused OR (i.kind='retest' AND (m.revision<>i.memory_revision OR m.memory_epoch<>i.memory_epoch)))))")
        .bind(auth.subject).bind(at).execute(&mut *tx).await.map_err(AppError::internal)?;
    // One active batch per user: another device resumes rather than reissuing short tests.
    let existing:Option<Uuid>=sqlx::query_scalar("SELECT s.id FROM learning_review_sessions s WHERE s.user_id=$1 AND s.expires_at>$2 AND s.settings_snapshot->>'english_variant'=$3 AND EXISTS(SELECT 1 FROM learning_review_items i WHERE i.session_id=s.id AND i.active AND i.expires_at>$2) ORDER BY s.created_at DESC LIMIT 1")
        .bind(auth.subject).bind(at).bind(&context.english_variant).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    if let Some(id) = existing {
        let existing_view = view_in(&mut tx, auth, id).await?;
        if existing_view.state == "active" {
            save_start(&mut tx, auth.subject, &input, &h, id).await?;
            crate::learning_tasks::repository::learner(&mut tx, auth).await?;
            tx.commit().await.map_err(AppError::internal)?;
            return Ok(existing_view);
        }
        sqlx::query("UPDATE learning_review_items SET active=false WHERE session_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    }
    let mut selected =
        queries::selected(&mut tx, auth.subject, &context, "retest", at, 100).await?;
    if input.mode == ReviewMode::Due {
        selected.extend(queries::selected(&mut tx, auth.subject, &context, "due", at, 100).await?);
    }
    let lists: Vec<_> = selected.iter().map(|s| s.wordlist_id).collect();
    let entries: Vec<_> = selected.iter().map(|s| s.entry_id).collect();
    let visible =
        crate::learning_tasks::repository::sources_for_entries(&mut tx, auth, &lists, &entries)
            .await?;
    let mut prepared = Vec::new();
    for mut m in selected {
        if !visible.available(&m.question()) {
            continue;
        }
        m.publication_id = visible.entries[&m.entry_id].publication_id;
        let candidate = m.candidate(&context, &visible)?;
        sqlx::query("UPDATE learning_memories SET content_available=$2,checked_publication_id=$3,checked_cefr_level=$4 WHERE id=$1")
            .bind(m.id).bind(candidate.is_some()).bind(m.publication_id).bind(&context.cefr_level)
            .execute(&mut *tx).await.map_err(AppError::internal)?;
        let Some(c) = candidate else {
            continue;
        };
        let sig = signature(
            &m.question(),
            &context,
            &c.prompt,
            &c.answer,
            GENERATION_VERSION,
            GRADING_VERSION,
        )?;
        let changed = sig != m.content_signature;
        let epoch = m.memory_epoch + i64::from(changed);
        let revision = m.revision + i64::from(changed);
        if changed {
            m.stage = Stage::Unverified;
            sqlx::query("UPDATE learning_memories SET memory_epoch=$2,content_signature=$3,stage='unverified',interval_step=0,due_at=$4,last_feedback_at=NULL,revision=$5,content_available=true WHERE id=$1")
                .bind(m.id).bind(epoch).bind(&sig).bind(at).bind(revision).execute(&mut *tx).await.map_err(AppError::internal)?;
        }
        prepared.push((m, c, sig, epoch, revision));
    }
    // Interleave at most half a normal batch with relearning, then fill unused slots.
    let mut order: Vec<usize> = Vec::new();
    if input.mode == ReviewMode::Due {
        order.extend(
            prepared
                .iter()
                .enumerate()
                .filter(|(_, x)| x.0.stage == Stage::Relearning)
                .take((limit as usize).div_ceil(2))
                .map(|(i, _)| i),
        );
        order.extend(
            prepared
                .iter()
                .enumerate()
                .filter(|(_, x)| x.0.stage != Stage::Relearning)
                .take(limit as usize - order.len())
                .map(|(i, _)| i),
        );
    }
    for i in 0..prepared.len() {
        if order.len() >= limit as usize {
            break;
        }
        if !order.contains(&i) {
            order.push(i);
        }
    }
    if order.is_empty() {
        // Persist the bounded availability scan so invalid content cannot pin the queue.
        tx.commit().await.map_err(AppError::internal)?;
        return Err(conflict(
            "当前批次没有可出题内容，已更新来源状态，请刷新复习安排",
        ));
    }
    let id = Uuid::now_v7();
    let expires = at + Duration::hours(24);
    sqlx::query("INSERT INTO learning_review_sessions(id,user_id,mode,settings_snapshot,created_at,expires_at) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(id).bind(auth.subject).bind(input.mode.as_str()).bind(serde_json::to_value(&context).map_err(AppError::internal)?).bind(at).bind(expires).execute(&mut *tx).await.map_err(AppError::internal)?;
    for (position, index) in order.into_iter().enumerate() {
        let (m, c, sig, epoch, revision) = &prepared[index];
        let retest = m.stage == Stage::Relearning;
        if retest {
            sqlx::query("UPDATE learning_memories SET retest_issued=CASE WHEN retest_day=$2 THEN retest_issued+1 ELSE 1 END,retest_day=$2 WHERE id=$1")
                .bind(m.id).bind(day).execute(&mut *tx).await.map_err(AppError::internal)?;
        }
        sqlx::query("INSERT INTO learning_review_items(id,session_id,user_id,memory_id,memory_revision,memory_epoch,content_signature,source_wordlist_id,source_membership_id,source_public_generation,entry_id,entry_archive_generation,pos_id,sense_id,publication_id,prompt_snapshot,answer_snapshot,generation_version,grading_version,position,kind,issued_day,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23)")
            .bind(Uuid::now_v7()).bind(id).bind(auth.subject).bind(m.id).bind(revision).bind(epoch).bind(sig)
            .bind(m.wordlist_id).bind(m.membership_id).bind(m.public_generation).bind(m.entry_id).bind(m.entry_archive_generation)
            .bind(m.pos_id).bind(m.sense_id).bind(m.publication_id).bind(serde_json::to_value(&c.prompt).map_err(AppError::internal)?)
            .bind(serde_json::to_value(&c.answer).map_err(AppError::internal)?).bind(GENERATION_VERSION).bind(GRADING_VERSION)
            .bind(position as i32).bind(if retest{"retest"}else{"due"}).bind(day).bind(if retest{expires.min(day_end)}else{expires})
            .execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    save_start(&mut tx, auth.subject, &input, &h, id).await?;
    crate::learning_tasks::repository::learner(&mut tx, auth).await?;
    let issued = rows(&mut tx, id).await?;
    let final_access = access(&mut tx, auth, &issued).await?;
    let end = now(&mut tx).await?;
    if issued
        .iter()
        .any(|i| !final_access.available(&i.question()) || i.expires_at <= end)
    {
        return Err(conflict("来源或学习日已变化，请重新开始"));
    }
    tx.commit().await.map_err(AppError::internal)?;
    get(state, auth, id).await
}

pub(super) async fn reveal(
    state: &AppState,
    auth: &AuthUser,
    session_id: Uuid,
    item: Uuid,
) -> Result<ReviewSession, AppError> {
    queries::require_feature(state)?;
    let (mut tx, _) = queries::begin(&state.pool, auth).await?;
    if !queries::enabled(&mut tx, auth.subject).await? {
        return Err(conflict("个人记忆已暂停"));
    }
    session(&mut tx, auth.subject, session_id).await?;
    let items = rows(&mut tx, session_id).await?;
    let i = items
        .iter()
        .find(|i| i.id == item)
        .ok_or_else(|| AppError::not_found("题目不存在"))?;
    let available = access(&mut tx, auth, &items).await?;
    let at = now(&mut tx).await?;
    if !i.active || i.expires_at <= at || !available.available(&i.question()) {
        return Err(conflict("题目已失效"));
    }
    let current: (i64, i64, bool) = sqlx::query_as(
        "SELECT revision,memory_epoch,paused FROM learning_memories WHERE id=$1 FOR UPDATE",
    )
    .bind(i.memory_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    if current.2
        || (i.kind == "retest" && (current.0 != i.memory_revision || current.1 != i.memory_epoch))
    {
        return Err(conflict("本次重测机会已更新，请恢复进度"));
    }
    sqlx::query(
        "UPDATE learning_review_items SET revealed_at=COALESCE(revealed_at,$2) WHERE id=$1",
    )
    .bind(item)
    .bind(at)
    .execute(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    sqlx::query("UPDATE learning_memories SET last_feedback_at=$2 WHERE id=$1")
        .bind(i.memory_id)
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    crate::learning_tasks::repository::learner(&mut tx, auth).await?;
    let final_access = access(&mut tx, auth, &items).await?;
    if !final_access.available(&i.question()) || now(&mut tx).await? >= i.expires_at {
        return Err(conflict("题目已失效"));
    }
    tx.commit().await.map_err(AppError::internal)?;
    get(state, auth, session_id).await
}
async fn receipt(
    pool: &PgPool,
    auth: &AuthUser,
    session_id: Uuid,
    id: Uuid,
    state: &AppState,
) -> Result<ReviewReceipt, AppError> {
    let a: AttemptRow = sqlx::query_as(
        "SELECT * FROM learning_review_attempts WHERE id=$1 AND user_id=$2 AND session_id=$3",
    )
    .bind(id)
    .bind(auth.subject)
    .bind(session_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::internal)?;
    let due:DateTime<Utc>=sqlx::query_scalar("SELECT GREATEST(m.due_at,COALESCE(m.last_feedback_at+interval '10 minutes',m.due_at),CASE WHEN m.stage='relearning' AND m.retest_day=((clock_timestamp() AT TIME ZONE 'Asia/Shanghai')-interval '4 hours')::date AND m.retest_issued>=2 THEN ((m.retest_day+1)::timestamp+interval '4 hours') AT TIME ZONE 'Asia/Shanghai' ELSE m.due_at END) FROM learning_memories m JOIN learning_review_items i ON i.memory_id=m.id WHERE i.id=$1").bind(a.item_id).fetch_one(pool).await.map_err(AppError::internal)?;
    Ok(ReviewReceipt {
        answer_id: a.id,
        item_id: a.item_id,
        is_correct: a.is_correct,
        memory_effect: a.effect,
        next_due_at: due,
        accepted_at: a.accepted_at,
        session: get(state, auth, session_id).await?,
    })
}
pub(super) async fn answer(
    state: &AppState,
    auth: &AuthUser,
    session_id: Uuid,
    input: ReviewAnswerInput,
) -> Result<ReviewReceipt, AppError> {
    queries::require_feature(state)?;
    let h = hash(&input)?;
    let (mut tx, _) = queries::begin(&state.pool, auth).await?;
    session(&mut tx, auth.subject, session_id).await?;
    let prior:Option<(Uuid,Vec<u8>,Uuid)>=sqlx::query_as("SELECT id,request_hash,session_id FROM learning_review_attempts WHERE user_id=$1 AND request_key=$2").bind(auth.subject).bind(input.idempotency_key).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    if let Some((id, old, stored_session)) = prior {
        if old != h || stored_session != session_id {
            return Err(conflict("请求键已用于其他答案"));
        }
        tx.commit().await.map_err(AppError::internal)?;
        return receipt(&state.pool, auth, session_id, id, state).await;
    }
    if !queries::enabled(&mut tx, auth.subject).await? {
        return Err(conflict("个人记忆已暂停"));
    }
    let s = session(&mut tx, auth.subject, session_id).await?;
    let items = rows(&mut tx, session_id).await?;
    let i = items
        .iter()
        .find(|i| i.id == input.item_id)
        .ok_or_else(|| AppError::not_found("题目不存在"))?;
    let available = access(&mut tx, auth, &items).await?;
    let at = now(&mut tx).await?;
    if !i.active
        || i.expires_at <= at
        || !available.available(&i.question())
        || i.grading_version != GRADING_VERSION
    {
        return Err(conflict("题目已失效，请恢复最新复习进度"));
    }
    let context: LearningContext =
        serde_json::from_value(s.settings_snapshot).map_err(AppError::internal)?;
    let current_signature = current_signature(&i.question(), &context, &available)?;
    let m:Memory=sqlx::query_as("SELECT id,memory_epoch,content_signature,stage,interval_step,due_at,last_feedback_at,revision FROM learning_memories WHERE id=$1 FOR UPDATE").bind(i.memory_id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    let paused: bool = sqlx::query_scalar("SELECT paused FROM learning_memories WHERE id=$1")
        .bind(i.memory_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    if paused
        || (i.kind == "retest"
            && (m.revision != i.memory_revision || m.memory_epoch != i.memory_epoch))
    {
        return Err(conflict("本次重测机会已更新，请恢复进度"));
    }
    let snapshot: AnswerSnapshot =
        serde_json::from_value(i.answer_snapshot.clone()).map_err(AppError::internal)?;
    let (_, correct) = grade_spelling(&snapshot, &input.answer)?;
    let stale = m.revision != i.memory_revision || m.memory_epoch != i.memory_epoch;
    let content_changed = current_signature.as_ref() != Some(&i.content_signature);
    let (mut schedule, effect) = if stale {
        (m.schedule(), "practice_only_stale")
    } else if content_changed {
        (m.schedule(), "practice_only_content_changed")
    } else if i.revealed_at.is_some() {
        let mut unchanged = m.schedule();
        unchanged.last_feedback_at = Some(at);
        (unchanged, "practice_assisted")
    } else {
        let (s, e) = advance(Some(&m.schedule()), correct, at);
        (s, e.as_str())
    };
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO learning_review_attempts(id,user_id,item_id,session_id,request_key,request_hash,submitted_answer,is_correct,effect,accepted_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
        .bind(id).bind(auth.subject).bind(i.id).bind(session_id).bind(input.idempotency_key).bind(h).bind(&input.answer).bind(correct).bind(effect).bind(at).execute(&mut *tx).await.map_err(AppError::internal)?;
    let after = m.revision + i64::from(!stale && !content_changed);
    // Feedback exposes the answer even when this evidence cannot advance memory.
    // Keep its cooldown without overwriting a newer schedule or revision.
    schedule.last_feedback_at = Some(at);
    sqlx::query("UPDATE learning_memories SET stage=$2,interval_step=$3,due_at=$4,last_feedback_at=$5,last_evidence_at=$6,revision=$7 WHERE id=$1")
        .bind(m.id).bind(schedule.stage.as_str()).bind(schedule.interval_step).bind(schedule.due_at).bind(schedule.last_feedback_at).bind(at).bind(after).execute(&mut *tx).await.map_err(AppError::internal)?;
    sqlx::query("UPDATE learning_review_items SET active=false WHERE id=$1")
        .bind(i.id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("INSERT INTO learning_memory_events(answer_id,user_id,memory_id,memory_epoch,previous_revision,resulting_revision,content_signature,scheduler_version,effect,is_correct,accepted_at,source_kind) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'review')")
        .bind(id).bind(auth.subject).bind(m.id).bind(m.memory_epoch).bind(m.revision).bind(after).bind(&i.content_signature).bind(SCHEDULER_VERSION).bind(effect).bind(correct).bind(at).execute(&mut *tx).await.map_err(AppError::internal)?;
    let final_access = access(&mut tx, auth, &items).await?;
    if !final_access.available(&i.question()) || now(&mut tx).await? >= i.expires_at {
        return Err(conflict("题目已失效，作答未保存"));
    }
    crate::learning_tasks::repository::learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    receipt(&state.pool, auth, session_id, id, state).await
}
