use super::{algorithm, dto::BankQuestionContent, source};
use crate::{
    auth::extract::AuthUser,
    error::AppError,
    learning_tasks::{
        dto::{LearningContext, LearningOption},
        question::{AnswerSnapshot, CandidatePool, LearningSource},
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
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use uuid::Uuid;
pub const CHOICE: &str = "choice_zh_to_en";
pub const GRADING: &str = "choice_id_v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceSnapshot {
    #[serde(default)]
    pub deferred: bool,
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
    let choice_capable = capable(request.headers());
    let mastery_capable = crate::learning_tasks::mastery::capable(request.headers());
    if choice_capable && mastery_capable {
        return Ok(run_with_vary(request, next).await);
    }
    let path = request.uri().path();
    let mut parts = path.trim_start_matches('/').split('/');
    if parts.next() != Some("api") || parts.next() != Some("v1") || parts.next() != Some("me") {
        return Ok(run_with_vary(request, next).await);
    }
    let resource = parts.next().unwrap_or("");
    let id = parts.next().and_then(|s| s.parse::<Uuid>().ok());
    if !["learning-plans", "learning-tasks", "learning-runs"].contains(&resource) || id.is_none() {
        return Ok(run_with_vary(request, next).await);
    }
    let is_run = resource == "learning-runs";
    let (mut request_parts, body) = request.into_parts();
    let auth = AuthUser::from_request_parts(&mut request_parts, &state).await?;
    let sql = if is_run {
        "SELECT r.question_type,t.generation_policy FROM learning_runs r JOIN learning_tasks t ON t.id=r.task_id WHERE r.id=$1 AND r.user_id=$2"
    } else {
        "SELECT question_type,generation_policy FROM learning_tasks WHERE id=$1 AND user_id=$2"
    };
    let kind: Option<(String, String)> = sqlx::query_as(sql)
        .bind(id)
        .bind(auth.subject)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?;
    if kind.as_ref().is_some_and(|(kind, _)| kind == CHOICE) && !choice_capable {
        return Err(super::conflict("此客户端不支持单选题，请更新后继续"));
    }
    if kind
        .as_ref()
        .is_some_and(|(_, mode)| mode == crate::learning_tasks::mastery::MODE)
        && !mastery_capable
    {
        return Err(super::conflict("请更新客户端后使用按掌握情况练习"));
    }
    Ok(run_with_vary(Request::from_parts(request_parts, body), next).await)
}
async fn run_with_vary(request: Request, next: Next) -> Response {
    let learning = request.uri().path().starts_with("/api/v1/me/learning-");
    let mut response = next.run(request).await;
    if learning {
        response.headers_mut().append(
            axum::http::header::VARY,
            axum::http::HeaderValue::from_static("X-TSZ-Learning-Capabilities"),
        );
    }
    response
}
pub async fn fingerprints(tx: &mut Tx<'_>, entries: &[Uuid]) -> Result<HashSet<String>, AppError> {
    let mut result = HashSet::new();
    for profile in ["BrE", "AmE"] {
        for chunk in entries.chunks(64) {
            result.extend(
                source::candidates(tx, chunk, "C2", profile)
                    .await?
                    .into_iter()
                    .map(|c| c.content_fingerprint),
            );
        }
    }
    Ok(result)
}
pub async fn build_pool(
    tx: &mut Tx<'_>,
    sources: &[LearningSource],
    settings: &LearningContext,
) -> Result<CandidatePool, AppError> {
    let mut pool = crate::learning_tasks::question::build_candidates(sources, settings)?;
    let mut ids: Vec<_> = sources.iter().map(|s| s.entry_id).collect();
    ids.sort();
    ids.dedup();
    let mut targets = Vec::new();
    for chunk in ids.chunks(64) {
        targets.extend(
            source::candidates(tx, chunk, &settings.cefr_level, &settings.english_variant).await?,
        );
    }
    let parts: Vec<_> = targets
        .iter()
        .map(|t| t.part_of_speech.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut algorithm =
        algorithm::Pool::load(tx, &settings.cefr_level, &settings.english_variant, &parts).await?;
    let mut eligible = Vec::new();
    for mut candidate in pool.candidates {
        let Some(target) = targets.iter().find(|t| {
            t.source.entry_id == candidate.entry_id
                && t.source.publication_id == candidate.publication_id
                && t.source.sense_id == candidate.sense_id
                && t.source.definition_id == candidate.definition_id
                && candidate.form_ids.contains(&t.source.form_id)
        }) else {
            continue;
        };
        let Some(content) = algorithm.content(tx, target, &settings.cefr_level).await? else {
            continue;
        };
        if content.prompt.chars().count() > 2000 || content.explanation.chars().count() > 2000 {
            continue;
        }
        if target.accepted_spellings.iter().any(|s| {
            crate::learning_tasks::question::leaks(
                &content.prompt,
                &crate::platform::text::key_v1(s),
            )
        }) {
            continue;
        }
        let digest =
            Sha256::digest(serde_json::to_vec(&target.source).map_err(AppError::internal)?);
        let mut identity = [0; 16];
        identity.copy_from_slice(&digest[..16]);
        candidate.answer.choice = Some(ChoiceSnapshot {
            deferred: true,
            question_id: Uuid::from_bytes(identity),
            revision: 1,
            content,
            options: vec![],
            correct_option_id: String::new(),
        });
        eligible.push(candidate);
    }
    pool.candidates = eligible;
    Ok(pool)
}
/// Freeze options only for the learning units selected for this run, in the run transaction.
pub async fn materialize(
    tx: &mut Tx<'_>,
    units: &[&crate::learning_tasks::repository::QuestionRow],
    settings: &LearningContext,
) -> Result<(), AppError> {
    let mut parts = Vec::new();
    for unit in units {
        if let Some(value) = unit.answer_snapshot.get("choice").filter(|v| !v.is_null()) {
            let c: ChoiceSnapshot =
                serde_json::from_value(value.clone()).map_err(AppError::internal)?;
            if c.deferred {
                parts.push(c.content.target.part_of_speech);
            }
        }
    }
    if parts.is_empty() {
        return Ok(());
    }
    let mut algorithm =
        algorithm::Pool::load(tx, &settings.cefr_level, &settings.english_variant, &parts).await?;
    for unit in units {
        let mut answer: AnswerSnapshot =
            serde_json::from_value(unit.answer_snapshot.clone()).map_err(AppError::internal)?;
        let Some(c) = answer.choice.as_mut().filter(|c| c.deferred) else {
            continue;
        };
        let content = algorithm
            .content(tx, &c.content.target, &settings.cefr_level)
            .await?
            .ok_or_else(|| {
                super::conflict("当前词库没有足够可靠且不含已知歧义的干扰项，请检查来源后重试")
            })?;
        source::validate_question(tx, &content).await?;
        c.options = content
            .options
            .iter()
            .map(|o| LearningOption {
                option_id: o.option_id.clone(),
                spelling: o.candidate.spelling.clone(),
            })
            .collect();
        c.correct_option_id = content.correct_option_id.clone();
        c.content = content;
        c.deferred = false;
        shuffle_answer(&mut answer);
        sqlx::query("UPDATE learning_cycle_units SET answer_snapshot=$2 WHERE id=$1")
            .bind(unit.id)
            .bind(serde_json::to_value(answer).map_err(AppError::internal)?)
            .execute(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    }
    Ok(())
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
