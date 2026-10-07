use super::{
    dto::*,
    question::{AnswerSnapshot, LearningSource},
};
use crate::{
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::HashMap;
use uuid::Uuid;
pub type Tx<'a> = Transaction<'a, Postgres>;
pub fn conflict(message: &str) -> AppError {
    AppError::conflict(ErrorCode::LearningTaskConflict, None, message)
}
pub fn unavailable() -> AppError {
    AppError::conflict(
        ErrorCode::LearningRunUnavailable,
        None,
        "来源已失效、任务已结束或轮次不可继续，请查看进度后明确重新开始",
    )
}
pub fn invalid(message: &str) -> AppError {
    AppError::bad_request(ErrorCode::InvalidRequestBody, message)
}
pub async fn now(tx: &mut Tx<'_>) -> Result<DateTime<Utc>, AppError> {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)
}
pub async fn learner(tx: &mut Tx<'_>, auth: &AuthUser) -> Result<LearningContext, AppError> {
    let row: Option<(String, Option<String>, i64)> =
        sqlx::query_as("SELECT status,phone,security_version FROM users WHERE id=$1")
            .bind(auth.subject)
            .fetch_optional(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    if !row
        .as_ref()
        .is_some_and(|r| r.0 == "active" && r.2 == auth.security_version)
    {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "会话已失效",
        ));
    }
    if row.is_some_and(|r| r.1.is_none()) {
        return Err(AppError::forbidden(
            ErrorCode::PhoneBindingRequired,
            "请先绑定手机号",
        ));
    }
    crate::account_deletion::ensure_not_effective_in(tx, auth.subject).await?;
    let student: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM user_roles WHERE user_id=$1 AND role='student')",
    )
    .bind(auth.subject)
    .fetch_one(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    if !student {
        return Err(AppError::forbidden(ErrorCode::Forbidden, "需要学生资格"));
    }
    let settings:Option<(String,String)>=sqlx::query_as("SELECT cefr_level,english_variant FROM student_profiles WHERE user_id=$1 AND cefr_level IS NOT NULL").bind(auth.subject).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
    let (cefr_level, english_variant) = settings.ok_or_else(|| {
        AppError::conflict(
            ErrorCode::LearningSettingsRequired,
            None,
            "请先完成学习设置",
        )
    })?;
    Ok(LearningContext {
        cefr_level,
        english_variant,
    })
}
/// All account locks precede task/run/source locks. Owners are immutable on wordlists;
/// missing owners/lists are handled by source availability, never retried indefinitely.
pub async fn begin<'a>(
    pool: &'a PgPool,
    auth: &AuthUser,
    lists: &[Uuid],
) -> Result<Tx<'a>, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let mut accounts: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT owner_user_id FROM wordlists WHERE id=ANY($1)")
            .bind(lists)
            .fetch_all(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    accounts.push(auth.subject);
    accounts.sort();
    accounts.dedup();
    sqlx::query("SELECT id FROM users WHERE id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(accounts)
        .fetch_all(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    learner(&mut tx, auth).await?;
    Ok(tx)
}
pub async fn task(tx: &mut Tx<'_>, auth: &AuthUser, id: Uuid) -> Result<LearningTask, AppError> {
    sqlx::query_as("SELECT id,name,task_type,wordlist_ids,daily_question_count,ends_at,state,revision,created_at FROM learning_tasks WHERE id=$1 AND user_id=$2 FOR UPDATE").bind(id).bind(auth.subject).fetch_optional(&mut **tx).await.map_err(AppError::internal)?.ok_or_else(||AppError::not_found("任务不存在"))
}
pub async fn task_lists(pool: &PgPool, auth: &AuthUser, id: Uuid) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar("SELECT wordlist_ids FROM learning_tasks WHERE id=$1 AND user_id=$2")
        .bind(id)
        .bind(auth.subject)
        .fetch_optional(pool)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("任务不存在"))
}
pub async fn run_task(pool: &PgPool, auth: &AuthUser, id: Uuid) -> Result<Uuid, AppError> {
    sqlx::query_scalar("SELECT task_id FROM learning_runs WHERE id=$1 AND user_id=$2")
        .bind(id)
        .bind(auth.subject)
        .fetch_optional(pool)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("轮次不存在"))
}
#[derive(sqlx::FromRow)]
pub struct RunRow {
    pub id: Uuid,
    pub task_id: Uuid,
    pub task_type: LearningTaskType,
    pub task_revision: i64,
    pub business_day: Option<NaiveDate>,
    pub expires_at: Option<DateTime<Utc>>,
    pub settings_snapshot: serde_json::Value,
    pub target_count: i32,
    pub state: LearningRunState,
    pub generation_version: String,
    pub grading_version: String,
    pub started_at: DateTime<Utc>,
}
pub async fn run(tx: &mut Tx<'_>, auth: &AuthUser, id: Uuid) -> Result<RunRow, AppError> {
    sqlx::query_as("SELECT * FROM learning_runs WHERE id=$1 AND user_id=$2 FOR UPDATE")
        .bind(id)
        .bind(auth.subject)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("轮次不存在"))
}
#[derive(sqlx::FromRow)]
pub struct QuestionRow {
    pub id: Uuid,
    pub position: i32,
    pub source_wordlist_id: Uuid,
    pub source_membership_id: Uuid,
    pub source_public_generation: Option<i64>,
    pub entry_id: Uuid,
    pub entry_archive_generation: i64,
    pub prompt_snapshot: serde_json::Value,
    pub answer_snapshot: serde_json::Value,
}
pub async fn questions(tx: &mut Tx<'_>, id: Uuid) -> Result<Vec<QuestionRow>, AppError> {
    sqlx::query_as("SELECT * FROM learning_questions WHERE run_id=$1 ORDER BY position")
        .bind(id)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)
}
#[derive(sqlx::FromRow)]
pub struct AnswerRow {
    pub id: Uuid,
    pub question_id: Uuid,
    pub submitted_answer: String,
    pub is_correct: bool,
    pub accepted_at: DateTime<Utc>,
}
pub async fn answers(tx: &mut Tx<'_>, id: Uuid) -> Result<HashMap<Uuid, AnswerRow>, AppError> {
    let rows: Vec<AnswerRow> = sqlx::query_as("SELECT * FROM learning_answers WHERE run_id=$1")
        .bind(id)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    Ok(rows.into_iter().map(|r| (r.question_id, r)).collect())
}
pub struct SourceAccess {
    pub lists: HashMap<Uuid, (i64, Option<i64>)>,
    pub entries: HashMap<Uuid, (Uuid, i64, serde_json::Value)>,
    pub members: HashMap<(Uuid, Uuid), Uuid>,
    pub ordered: Vec<(Uuid, Uuid)>,
}
pub async fn sources(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    ids: &[Uuid],
) -> Result<SourceAccess, AppError> {
    source_access(tx, auth, ids, None).await
}
pub async fn sources_for_questions(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    ids: &[Uuid],
    questions: &[QuestionRow],
) -> Result<SourceAccess, AppError> {
    let entries: Vec<_> = questions.iter().map(|q| q.entry_id).collect();
    source_access(tx, auth, ids, Some(&entries)).await
}
async fn source_access(
    tx: &mut Tx<'_>,
    auth: &AuthUser,
    ids: &[Uuid],
    fixed_entries: Option<&[Uuid]>,
) -> Result<SourceAccess, AppError> {
    let rows:Vec<(Uuid,Uuid,String,i64,i64)>=sqlx::query_as("SELECT id,owner_user_id,state,revision,learning_public_generation FROM wordlists WHERE id=ANY($1) ORDER BY id FOR SHARE").bind(ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let mut lists = HashMap::new();
    for (id, owner, state, revision, generation) in rows {
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users u WHERE id=$1 AND status='active' AND NOT EXISTS(SELECT 1 FROM account_deletion_requests d WHERE d.user_id=u.id AND d.status='pending' AND d.effective_at<=clock_timestamp()))").bind(owner).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
        if active && (owner == auth.subject || state == "published") {
            lists.insert(
                id,
                (revision, (owner != auth.subject).then_some(generation)),
            );
        }
    }
    let visible_ids: Vec<_> = ids
        .iter()
        .copied()
        .filter(|id| lists.contains_key(id))
        .collect();
    let members:Vec<(Uuid,Uuid,Uuid)>=sqlx::query_as("SELECT wordlist_id,entry_id,learning_membership_id FROM wordlist_items WHERE wordlist_id=ANY($1) AND ($2::uuid[] IS NULL OR entry_id=ANY($2)) ORDER BY array_position($1,wordlist_id),position").bind(visible_ids).bind(fixed_entries).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let mut entry_ids: Vec<_> = members.iter().map(|(_, e, _)| *e).collect();
    entry_ids.sort();
    entry_ids.dedup();
    if entry_ids.len() > 1000 {
        return Err(invalid("来源超过 1000 个词条，请拆分任务"));
    }
    crate::wordlists::service::lock_entries(tx, &entry_ids, false).await?;
    let entries:Vec<(Uuid,Uuid,i64,serde_json::Value)>=sqlx::query_as("SELECT e.id,p.id,e.wordlist_archive_generation,p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=ANY($1) AND e.archived_at IS NULL AND p.content_schema_version=3").bind(entry_ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    // Entry locks may have waited across an owner's deletion deadline. Project only
    // after rechecking all owners with the database clock; these account locks are held.
    let visible: Vec<Uuid> = sqlx::query_scalar("SELECT w.id FROM wordlists w JOIN users u ON u.id=w.owner_user_id WHERE w.id=ANY($1) AND u.status='active' AND NOT EXISTS(SELECT 1 FROM account_deletion_requests d WHERE d.user_id=u.id AND d.status='pending' AND d.effective_at<=clock_timestamp())").bind(ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    lists.retain(|id, _| visible.contains(id));
    Ok(SourceAccess {
        lists,
        entries: entries
            .into_iter()
            .map(|(e, p, g, s)| (e, (p, g, s)))
            .collect(),
        members: members.iter().map(|(l, e, m)| ((*l, *e), *m)).collect(),
        ordered: members.into_iter().map(|(l, e, _)| (l, e)).collect(),
    })
}
impl SourceAccess {
    pub fn available(&self, q: &QuestionRow) -> bool {
        self.lists
            .get(&q.source_wordlist_id)
            .is_some_and(|(_, generation)| *generation == q.source_public_generation)
            && self.members.get(&(q.source_wordlist_id, q.entry_id))
                == Some(&q.source_membership_id)
            && self
                .entries
                .get(&q.entry_id)
                .is_some_and(|(_, g, _)| *g == q.entry_archive_generation)
    }
    pub fn learning_sources(&self) -> Result<Vec<LearningSource>, AppError> {
        self.ordered
            .iter()
            .filter_map(|(list, entry)| {
                self.lists.get(list).zip(self.entries.get(entry)).map(
                    |((revision, generation), (p, g, s))| {
                        (list, entry, revision, generation, p, g, s)
                    },
                )
            })
            .map(|(l, e, r, generation, p, g, s)| {
                Ok(LearningSource {
                    wordlist_id: *l,
                    revision: *r,
                    membership_id: self.members[&(*l, *e)],
                    public_generation: *generation,
                    entry_id: *e,
                    publication_id: *p,
                    archive_generation: *g,
                    word: serde_json::from_value(s.clone()).map_err(AppError::internal)?,
                })
            })
            .collect()
    }
}
pub fn question_view(
    q: &QuestionRow,
    a: Option<&AnswerRow>,
    access: &SourceAccess,
) -> Result<LearningQuestion, AppError> {
    let visible = access.available(q);
    let feedback = if visible {
        a.map(|a| {
            let snapshot: AnswerSnapshot =
                serde_json::from_value(q.answer_snapshot.clone()).map_err(AppError::internal)?;
            Ok::<_, AppError>(LearningAnswerFeedback {
                answer_id: a.id,
                submitted_answer: a.submitted_answer.clone(),
                is_correct: a.is_correct,
                accepted_at: a.accepted_at,
                accepted_answers: snapshot.answers,
            })
        })
        .transpose()?
    } else {
        None
    };
    Ok(LearningQuestion {
        id: q.id,
        position: q.position,
        answered: a.is_some(),
        content_available: visible,
        prompt: if visible {
            Some(serde_json::from_value(q.prompt_snapshot.clone()).map_err(AppError::internal)?)
        } else {
            None
        },
        feedback,
    })
}
pub async fn run_view(
    tx: &mut Tx<'_>,
    row: &RunRow,
    qs: &[QuestionRow],
    access: &SourceAccess,
) -> Result<LearningRun, AppError> {
    let time = now(tx).await?;
    let (answered_count, correct_count): (i64, i64) = sqlx::query_as(
        "SELECT count(*),count(*) FILTER(WHERE is_correct) FROM learning_answers WHERE run_id=$1",
    )
    .bind(row.id)
    .fetch_one(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    let completion_id = sqlx::query_scalar("SELECT id FROM learning_completions WHERE run_id=$1")
        .bind(row.id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    let state = if row.state == LearningRunState::Active {
        if qs.iter().any(|q| !access.available(q)) {
            LearningRunState::Invalidated
        } else if row.expires_at.is_some_and(|e| e <= time) {
            LearningRunState::Expired
        } else {
            row.state
        }
    } else {
        row.state
    };
    Ok(LearningRun {
        id: row.id,
        task_id: row.task_id,
        task_type: row.task_type,
        state,
        question_type: "spelling_zh_to_en".into(),
        task_revision: row.task_revision,
        business_day: row.business_day,
        expires_at: row.expires_at,
        started_at: row.started_at,
        server_time: time,
        settings: serde_json::from_value(row.settings_snapshot.clone())
            .map_err(AppError::internal)?,
        target_count: row.target_count,
        answered_count,
        correct_count,
        generation_version: row.generation_version.clone(),
        grading_version: row.grading_version.clone(),
        completion_id,
    })
}
pub fn page(query: &LearningPageQuery) -> Result<(u32, u32), AppError> {
    let p = query.page.unwrap_or(1);
    let s = query.page_size.unwrap_or(20);
    if p == 0 || !(1..=50).contains(&s) {
        return Err(AppError::bad_request(
            ErrorCode::InvalidQuery,
            "分页范围无效",
        ));
    }
    Ok((p, s))
}
