//! Personal spelling memory consumes committed-in-the-same-transaction first answers.
//! It never completes a task or participates in reward eligibility.
pub(crate) mod dto;
pub(crate) mod handler;
mod queries;
pub(crate) mod retention;
mod review;
mod scheduler;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{
    error::AppError,
    learning_tasks::{
        dto::{LearningContext, LearningPrompt},
        question::{
            AnswerSnapshot, GENERATION_VERSION, GRADING_VERSION, LearningSource, build_candidates,
        },
        repository::{QuestionRow, RunRow, SourceAccess},
    },
    wordlists::service::hash,
};
use scheduler::{SCHEDULER_VERSION, Schedule, Stage, advance};

type Tx<'a> = Transaction<'a, Postgres>;
const SKILL: &str = "spelling_zh_to_en";

#[derive(sqlx::FromRow)]
struct Memory {
    id: Uuid,
    memory_epoch: i64,
    content_signature: Vec<u8>,
    stage: Stage,
    interval_step: i16,
    due_at: DateTime<Utc>,
    last_feedback_at: Option<DateTime<Utc>>,
    revision: i64,
}
impl Memory {
    fn schedule(&self) -> Schedule {
        Schedule {
            stage: self.stage,
            interval_step: self.interval_step,
            due_at: self.due_at,
            last_feedback_at: self.last_feedback_at,
        }
    }
}

#[derive(Serialize)]
struct ContentIdentity<'a> {
    version: i16,
    entry_id: Option<Uuid>,
    pos_id: Option<Uuid>,
    sense_id: Option<Uuid>,
    skill: &'static str,
    answer_profile: &'a str,
    prompt: &'a LearningPrompt,
    answer_keys: Vec<&'a str>,
    normalization_version: i16,
    generation_version: &'a str,
    grading_version: &'a str,
}
fn signature(
    question: &QuestionRow,
    context: &LearningContext,
    prompt: &LearningPrompt,
    answer: &AnswerSnapshot,
    generation: &str,
    grading: &str,
) -> Result<Vec<u8>, AppError> {
    let mut answer_keys: Vec<_> = answer.keys.iter().map(String::as_str).collect();
    answer_keys.sort_unstable();
    answer_keys.dedup();
    hash(&ContentIdentity {
        version: 1,
        entry_id: question.entry_id,
        pos_id: question.pos_id,
        sense_id: question.sense_id,
        skill: SKILL,
        answer_profile: &context.english_variant,
        prompt,
        answer_keys,
        normalization_version: answer.normalization_version,
        generation_version: generation,
        grading_version: grading,
    })
}

fn current_signature(
    question: &QuestionRow,
    context: &LearningContext,
    access: &SourceAccess,
) -> Result<Option<Vec<u8>>, AppError> {
    let Some(entry_id) = question.entry_id else {
        return Ok(None);
    };
    let Some(publication) = access.entries.get(&entry_id) else {
        return Ok(None);
    };
    let source = LearningSource {
        wordlist_id: question.source_wordlist_id,
        revision: 0,
        membership_id: question.source_membership_id,
        public_generation: question.source_public_generation,
        entry_id,
        archive_generation: publication.archive_generation,
        publication_id: publication.publication_id,
        content: publication.content_v3()?,
    };
    let pool = build_candidates(&[source], context)?;
    pool.candidates
        .iter()
        .find(|c| c.pos_id == question.pos_id && c.sense_id == question.sense_id)
        .map(|c| {
            signature(
                question,
                context,
                &c.prompt,
                &c.answer,
                GENERATION_VERSION,
                GRADING_VERSION,
            )
        })
        .transpose()
}

struct Event<'a> {
    answer_id: Uuid,
    user: Uuid,
    memory: Option<Uuid>,
    epoch: Option<i64>,
    before: Option<i64>,
    after: Option<i64>,
    signature: &'a [u8],
    effect: &'a str,
    correct: bool,
    at: DateTime<Utc>,
}
async fn write_event(tx: &mut Tx<'_>, event: Event<'_>) -> Result<(), AppError> {
    sqlx::query("INSERT INTO learning_memory_events(answer_id,user_id,memory_id,memory_epoch,previous_revision,resulting_revision,content_signature,scheduler_version,effect,is_correct,accepted_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
        .bind(event.answer_id).bind(event.user).bind(event.memory).bind(event.epoch)
        .bind(event.before).bind(event.after).bind(event.signature).bind(SCHEDULER_VERSION)
        .bind(event.effect).bind(event.correct).bind(event.at)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}

/// Caller holds the learner/account, task, source and entry locks in their existing order.
/// Enrollment changes must take that same learner lock before changing the row.
pub(crate) async fn record_answer(
    tx: &mut Tx<'_>,
    user: Uuid,
    run: &RunRow,
    question: &QuestionRow,
    answer_id: Uuid,
    access: &SourceAccess,
) -> Result<(), AppError> {
    if question.custom_item_id.is_some()
        || question
            .answer_snapshot
            .get("wordlist_fallback")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    {
        return Ok(());
    }
    let enabled: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM learning_memory_enrollments WHERE user_id=$1 AND enabled AND enabled_at<=clock_timestamp())")
        .bind(user).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
    if !enabled {
        return Ok(());
    }
    let seen: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM learning_memory_events WHERE answer_id=$1)",
    )
    .bind(answer_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    if seen {
        return Ok(());
    }
    let (correct, at): (bool, DateTime<Utc>) = sqlx::query_as(
        "SELECT is_correct,accepted_at FROM learning_answers WHERE id=$1 AND question_id=$2",
    )
    .bind(answer_id)
    .bind(question.id)
    .fetch_one(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    let enabled_at: DateTime<Utc> =
        sqlx::query_scalar("SELECT enabled_at FROM learning_memory_enrollments WHERE user_id=$1")
            .bind(user)
            .fetch_one(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    if at < enabled_at {
        return Ok(());
    }
    let context: LearningContext =
        serde_json::from_value(run.settings_snapshot.clone()).map_err(AppError::internal)?;
    let prompt: LearningPrompt =
        serde_json::from_value(question.prompt_snapshot.clone()).map_err(AppError::internal)?;
    let answer: AnswerSnapshot =
        serde_json::from_value(question.answer_snapshot.clone()).map_err(AppError::internal)?;
    let submitted_signature = signature(
        question,
        &context,
        &prompt,
        &answer,
        if run.generation_version == crate::learning_tasks::question::WORDLIST_GENERATION_VERSION {
            GENERATION_VERSION
        } else {
            &run.generation_version
        },
        &run.grading_version,
    )?;
    let current_signature = current_signature(question, &context, access)?;
    let current: Option<Memory> = sqlx::query_as("SELECT id,memory_epoch,content_signature,stage,interval_step,due_at,last_feedback_at,revision FROM learning_memories WHERE user_id=$1 AND entry_id=$2 AND pos_id=$3 AND sense_id=$4 AND skill=$5 AND answer_profile=$6 FOR UPDATE")
        .bind(user).bind(question.entry_id).bind(question.pos_id).bind(question.sense_id)
        .bind(SKILL).bind(&context.english_variant).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
    let matches_current = current_signature.as_ref() == Some(&submitted_signature);
    if current.is_none() && !matches_current {
        return write_event(
            tx,
            Event {
                answer_id,
                user,
                memory: None,
                epoch: None,
                before: None,
                after: None,
                signature: &submitted_signature,
                effect: "practice_only_content_changed",
                correct,
                at,
            },
        )
        .await;
    }
    let id = current.as_ref().map_or_else(Uuid::now_v7, |m| m.id);
    let before = current.as_ref().map_or(0, |m| m.revision);
    let changed = current.as_ref().is_some_and(|m| {
        current_signature
            .as_ref()
            .is_some_and(|s| *s != m.content_signature)
    });
    let epoch = current
        .as_ref()
        .map_or(1, |m| m.memory_epoch + i64::from(changed));
    let state_signature = current_signature.as_deref().unwrap_or_else(|| {
        current
            .as_ref()
            .expect("existing unavailable memory")
            .content_signature
            .as_slice()
    });
    let (schedule, effect) = if matches_current {
        let previous = current.as_ref().filter(|_| !changed).map(Memory::schedule);
        let (schedule, effect) = advance(previous.as_ref(), correct, at);
        (schedule, effect.as_str())
    } else {
        let schedule = if changed || current_signature.is_none() {
            Schedule {
                stage: Stage::Unverified,
                interval_step: 0,
                due_at: at,
                last_feedback_at: None,
            }
        } else {
            current.as_ref().expect("existing memory").schedule()
        };
        (schedule, "practice_only_content_changed")
    };
    if current.is_some() {
        sqlx::query("UPDATE learning_memories SET memory_epoch=$2,content_signature=$3,stage=$4,interval_step=$5,due_at=$6,last_feedback_at=$7,last_evidence_at=$8,revision=revision+1,content_available=$9 WHERE id=$1")
            .bind(id).bind(epoch).bind(state_signature).bind(schedule.stage.as_str()).bind(schedule.interval_step)
            .bind(schedule.due_at).bind(schedule.last_feedback_at).bind(at).bind(current_signature.is_some())
            .execute(&mut **tx).await.map_err(AppError::internal)?;
    } else {
        sqlx::query("INSERT INTO learning_memories(id,user_id,entry_id,pos_id,sense_id,skill,answer_profile,memory_epoch,content_signature,scheduler_version,stage,interval_step,due_at,last_feedback_at,last_evidence_at,revision) VALUES($1,$2,$3,$4,$5,$6,$7,1,$8,$9,$10,$11,$12,$13,$14,1)")
            .bind(id).bind(user).bind(question.entry_id).bind(question.pos_id).bind(question.sense_id)
            .bind(SKILL).bind(&context.english_variant).bind(state_signature).bind(SCHEDULER_VERSION)
            .bind(schedule.stage.as_str()).bind(schedule.interval_step).bind(schedule.due_at)
            .bind(schedule.last_feedback_at).bind(at).execute(&mut **tx).await.map_err(AppError::internal)?;
    }
    // A deleted wordlist's in-flight grace is not permission for a new review source.
    if access.members.get(&(
        question.source_wordlist_id,
        question.entry_id.expect("platform memory"),
    )) == Some(&question.source_membership_id)
        && !access.grace.contains_key(&question.source_wordlist_id)
    {
        sqlx::query("INSERT INTO learning_memory_sources(memory_id,user_id,wordlist_id,membership_id,public_generation,entry_archive_generation,observed_at) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(memory_id,wordlist_id) DO UPDATE SET membership_id=EXCLUDED.membership_id,public_generation=EXCLUDED.public_generation,entry_archive_generation=EXCLUDED.entry_archive_generation,observed_at=EXCLUDED.observed_at")
            .bind(id).bind(user).bind(question.source_wordlist_id).bind(question.source_membership_id)
            .bind(question.source_public_generation).bind(question.entry_archive_generation).bind(at)
            .execute(&mut **tx).await.map_err(AppError::internal)?;
    }
    write_event(
        tx,
        Event {
            answer_id,
            user,
            memory: Some(id),
            epoch: Some(epoch),
            before: Some(before),
            after: Some(before + 1),
            signature: &submitted_signature,
            effect,
            correct,
            at,
        },
    )
    .await
}
