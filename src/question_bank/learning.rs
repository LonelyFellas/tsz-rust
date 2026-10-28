use super::{dto::BankQuestionContent, source};
use crate::{
    auth::extract::AuthUser,
    error::AppError,
    learning_tasks::{
        dto::{LearningContext, LearningOption},
        question::{AnswerSnapshot, Candidate, CandidatePool, LearningSource},
        repository::Tx,
    },
    state::AppState,
};
use axum::{
    extract::{FromRequestParts, Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
use sqlx::types::Json;
use std::collections::HashSet;
use uuid::Uuid;
pub const CHOICE: &str = "choice_zh_to_en";
pub const GRADING: &str = "choice_id_v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceSnapshot {
    pub question_id: Uuid,
    pub revision: i64,
    pub content: BankQuestionContent,
    pub options: Vec<LearningOption>,
    pub correct_option_id: String,
}
pub fn capable(headers: &HeaderMap) -> bool {
    headers
        .get("x-tsz-learning-capabilities")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|x| x.trim() == "choice-v1"))
}
pub fn require_capability(
    headers: &HeaderMap,
    question_type: Option<&str>,
) -> Result<(), AppError> {
    if let Some(kind) = question_type {
        if !["spelling_zh_to_en", CHOICE].contains(&kind) {
            return Err(super::invalid("题型无效"));
        }
        if kind == CHOICE && !capable(headers) {
            return Err(super::conflict("此客户端不支持单选题，请更新后继续"));
        }
    }
    Ok(())
}
pub async fn capability_guard(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    if capable(request.headers()) {
        return Ok(next.run(request).await);
    }
    let path = request.uri().path();
    let mut parts = path.trim_start_matches('/').split('/');
    if parts.next() != Some("api") || parts.next() != Some("v1") || parts.next() != Some("me") {
        return Ok(next.run(request).await);
    }
    let resource = parts.next().unwrap_or("");
    let id = parts.next().and_then(|s| s.parse::<Uuid>().ok());
    if !["learning-plans", "learning-tasks", "learning-runs"].contains(&resource) || id.is_none() {
        return Ok(next.run(request).await);
    }
    let is_run = resource == "learning-runs";
    let (mut request_parts, body) = request.into_parts();
    let auth = AuthUser::from_request_parts(&mut request_parts, &state).await?;
    let sql = if is_run {
        "SELECT question_type FROM learning_runs WHERE id=$1 AND user_id=$2"
    } else {
        "SELECT question_type FROM learning_tasks WHERE id=$1 AND user_id=$2"
    };
    let kind: Option<String> = sqlx::query_scalar(sql)
        .bind(id)
        .bind(auth.subject)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?;
    if kind.as_deref() == Some(CHOICE) {
        return Err(super::conflict("此客户端不支持单选题，请更新后继续"));
    }
    Ok(next.run(Request::from_parts(request_parts, body)).await)
}
pub async fn available(
    tx: &mut Tx<'_>,
    entries: &[Uuid],
) -> Result<HashSet<(Uuid, i64)>, AppError> {
    let rows:Vec<(Uuid,i64,Json<BankQuestionContent>)>=sqlx::query_as("SELECT id,revision,content FROM question_bank WHERE state='approved' AND target_entry_id=ANY($1) ORDER BY id FOR SHARE").bind(entries).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let mut available = HashSet::new();
    for (id, revision, content) in rows {
        match source::validate_question(tx, &content).await {
            Ok(()) => {
                available.insert((id, revision));
            }
            Err(e) if e.status_code().is_client_error() => {}
            Err(e) => return Err(e),
        }
    }
    Ok(available)
}
pub async fn build_pool(
    tx: &mut Tx<'_>,
    sources: &[LearningSource],
    settings: &LearningContext,
) -> Result<CandidatePool, AppError> {
    let entries: Vec<Uuid> = sources.iter().map(|s| s.entry_id).collect();
    let rows:Vec<(Uuid,i64,Json<BankQuestionContent>)>=sqlx::query_as("SELECT id,revision,content FROM question_bank WHERE state='approved' AND target_entry_id=ANY($1) AND question_level=$2 AND answer_profile=$3 ORDER BY updated_at DESC,id DESC FOR SHARE").bind(&entries).bind(&settings.cefr_level).bind(&settings.english_variant).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let mut valid = vec![];
    for row in rows {
        match source::validate_question(tx, &row.2).await {
            Ok(()) => valid.push(row),
            Err(e) if e.status_code().is_client_error() => {}
            Err(e) => return Err(e),
        }
    }
    let mut candidates = vec![];
    let mut seen = HashSet::new();
    for source in sources {
        for (id, revision, content) in &valid {
            let target = &content.target.source;
            if target.entry_id != source.entry_id || target.publication_id != source.publication_id
            {
                continue;
            }
            let unit_key = format!("{}:{}:{}", target.entry_id, target.pos_id, target.sense_id);
            if !seen.insert(unit_key.clone()) {
                continue;
            }
            let options = content
                .options
                .iter()
                .map(|o| LearningOption {
                    option_id: o.option_id.clone(),
                    spelling: o.candidate.spelling.clone(),
                })
                .collect();
            candidates.push(Candidate {
                source_wordlist_id: source.wordlist_id,
                source_revision: source.revision,
                source_membership_id: source.membership_id,
                source_public_generation: source.public_generation,
                entry_id: source.entry_id,
                entry_archive_generation: source.archive_generation,
                publication_id: source.publication_id,
                pos_id: target.pos_id,
                sense_id: target.sense_id,
                definition_id: target.definition_id,
                form_ids: vec![target.form_id],
                unit_key,
                prompt: crate::learning_tasks::dto::LearningPrompt {
                    definition: content.prompt.clone(),
                    part_of_speech: content.target.part_of_speech.clone(),
                },
                answer: AnswerSnapshot {
                    answers: vec![content.target.spelling.clone()],
                    keys: vec![],
                    normalization_version: 1,
                    choice: Some(ChoiceSnapshot {
                        question_id: *id,
                        revision: *revision,
                        content: content.0.clone(),
                        options,
                        correct_option_id: content.correct_option_id.clone(),
                    }),
                },
            });
        }
    }
    Ok(CandidatePool {
        candidates,
        exclusions: Default::default(),
    })
}
pub fn shuffle_answer(value: &mut AnswerSnapshot) {
    if let Some(choice) = &mut value.choice {
        use rand::seq::SliceRandom;
        choice.options.shuffle(&mut rand::rng());
        // IDs are fresh and unrelated to bank position; A is never a signal for correctness.
        let old = choice.correct_option_id.clone();
        for option in &mut choice.options {
            let id = Uuid::now_v7().to_string();
            if option.option_id == old {
                choice.correct_option_id = id.clone();
            }
            option.option_id = id;
        }
    }
}
pub fn grade(snapshot: &AnswerSnapshot, input: &str) -> Result<(String, bool), AppError> {
    let choice = snapshot
        .choice
        .as_ref()
        .ok_or_else(|| super::conflict("题目快照缺失"))?;
    if !choice.options.iter().any(|o| o.option_id == input) {
        return Err(super::invalid("所选选项不存在"));
    }
    Ok((input.to_owned(), input == choice.correct_option_id))
}
