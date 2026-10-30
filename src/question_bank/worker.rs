use super::{dto::*, service, source};
use crate::{error::AppError, state::AppState};
use serde_json::{Value, json};
use sqlx::types::Json;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

pub struct AgentConfig {
    pub base_url: String,
    pub outbound_token: String,
    pub inbound_token: String,
    pub client: reqwest::Client,
}
impl AgentConfig {
    pub fn from_env() -> anyhow::Result<Option<Arc<Self>>> {
        let base = std::env::var("VOCABULARY_AGENT_URL")
            .ok()
            .filter(|v| !v.is_empty());
        let outbound = std::env::var("VOCABULARY_AGENT_TOKEN")
            .ok()
            .filter(|v| !v.is_empty());
        let inbound = std::env::var("VOCABULARY_AGENT_CALLBACK_TOKEN")
            .ok()
            .filter(|v| !v.is_empty());
        if base.is_none() && outbound.is_none() && inbound.is_none() {
            return Ok(None);
        }
        let base_url = base.ok_or_else(|| anyhow::anyhow!("VOCABULARY_AGENT_URL missing"))?;
        let url = reqwest::Url::parse(&base_url)?;
        anyhow::ensure!(
            url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))),
            "Agent URL must use HTTPS or loopback HTTP"
        );
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid Agent URL"
        );
        let outbound_token =
            outbound.ok_or_else(|| anyhow::anyhow!("VOCABULARY_AGENT_TOKEN missing"))?;
        let inbound_token =
            inbound.ok_or_else(|| anyhow::anyhow!("VOCABULARY_AGENT_CALLBACK_TOKEN missing"))?;
        anyhow::ensure!(
            outbound_token.len() >= 32
                && inbound_token.len() >= 32
                && outbound_token != inbound_token,
            "two distinct service credentials of at least 32 bytes required"
        );
        Ok(Some(Arc::new(Self {
            base_url: base_url.trim_end_matches('/').into(),
            outbound_token,
            inbound_token,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(8))
                .build()?,
        })))
    }
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        key: Option<Uuid>,
    ) -> Result<Value, AppError> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(&self.outbound_token);
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(serde_json::to_vec(&body).map_err(AppError::internal)?);
        }
        if let Some(key) = key {
            request = request.header("Idempotency-Key", key.to_string());
        }
        let response = request.send().await.map_err(AppError::internal)?;
        if !response.status().is_success() {
            return Err(super::conflict("Agent 服务请求未完成"));
        }
        let bytes = response.bytes().await.map_err(AppError::internal)?;
        if bytes.len() > 1_000_000 {
            return Err(super::invalid("Agent 响应过大"));
        }
        serde_json::from_slice(&bytes).map_err(AppError::internal)
    }
}
pub fn run_worker(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            if tick(&state).await.is_err() {
                tracing::warn!(event = "question_bank_delivery_retry");
            }
        }
    });
}
pub async fn tick(state: &AppState) -> Result<(), AppError> {
    generate_algorithmic(state).await?;
    let Some(config) = &state.vocabulary_agent else {
        return Ok(());
    };
    let mut claim = state.pool.begin().await.map_err(AppError::internal)?;
    super::batch::lock_creation(&mut claim).await?;
    let id: Option<Uuid> = sqlx::query_scalar(r#"
        WITH active AS (
          SELECT count(*) AS n FROM question_generations
          WHERE submitted_at IS NOT NULL AND (state IN ('queued','running') OR
            (state IN ('cancelled','failed') AND NOT cancel_delivered AND expires_at>now()))
        ), dispatch AS MATERIALIZED (
          SELECT batch_id,max(submitted_at) AS last_submitted FROM question_generations GROUP BY batch_id
        )
        UPDATE question_generations SET next_attempt_at=now()+interval '20 seconds',attempts=attempts+1,
          expires_at=CASE WHEN submitted_at IS NULL AND state='queued' THEN now()+interval '2 hours' ELSE expires_at END,
          submitted_at=CASE WHEN state='queued' THEN COALESCE(submitted_at,now()) ELSE submitted_at END
        WHERE id=(SELECT g.id FROM question_generations g LEFT JOIN dispatch d ON d.batch_id IS NOT DISTINCT FROM g.batch_id
          WHERE generation_method='agent' AND next_attempt_at<=now() AND (
            (state IN ('cancelled','failed') AND NOT cancel_delivered) OR
            (state IN ('queued','running') AND (submitted_at IS NOT NULL OR (SELECT n FROM active)<2)))
          ORDER BY CASE WHEN state IN ('cancelled','failed') THEN 0 WHEN submitted_at IS NOT NULL THEN 1 ELSE 2 END,
            CASE WHEN submitted_at IS NULL THEN COALESCE(d.last_submitted,'epoch'::timestamptz) ELSE next_attempt_at END,
            g.created_at,g.id FOR UPDATE OF g SKIP LOCKED LIMIT 1)
        RETURNING id
    "#).fetch_optional(&mut *claim).await.map_err(AppError::internal)?;
    claim.commit().await.map_err(AppError::internal)?;
    let generation_result = if let Some(id) = id {
        let result = process(state, config, id).await;
        if result.is_err() {
            sqlx::query("UPDATE question_generations SET error_code='agent_retry_pending',updated_at=now() WHERE id=$1 AND state IN ('queued','running')").bind(id).execute(&state.pool).await.map_err(AppError::internal)?;
        }
        result
    } else {
        Ok(())
    };
    let event:Option<(Uuid,Value)>=sqlx::query_as("UPDATE question_review_deliveries SET next_attempt_at=now()+interval '30 seconds',attempts=attempts+1 WHERE event_id=(SELECT event_id FROM question_review_deliveries WHERE delivered_at IS NULL AND next_attempt_at<=now() ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING event_id,payload").fetch_optional(&state.pool).await.map_err(AppError::internal)?;
    if let Some((id, payload)) = event {
        config
            .call(
                reqwest::Method::POST,
                "/v1/review-events",
                Some(payload),
                None,
            )
            .await?;
        sqlx::query("UPDATE question_review_deliveries SET delivered_at=now() WHERE event_id=$1")
            .bind(id)
            .execute(&state.pool)
            .await
            .map_err(AppError::internal)?;
    }
    generation_result
}
async fn generate_algorithmic(state: &AppState) -> Result<(), AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM question_generations WHERE generation_method='algorithm' AND state='queued' AND next_attempt_at<=now() ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1")
        .fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    let Some(id) = id else {
        return Ok(());
    };
    let current = match service::delegation(&mut tx, id).await {
        Ok(row) => row,
        Err(error) if !error.status_code().is_server_error() => {
            sqlx::query("UPDATE question_generations SET state='failed',error_code='authorization_or_source_changed',revision=revision+1,updated_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
            tx.commit().await.map_err(AppError::internal)?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let search = AgentCandidateSearch {
        contract_version: 1,
        target_source: current.target.source.clone(),
        question_level: current.question_level.clone(),
        answer_profile: current.answer_profile.clone(),
        exclude_candidate_refs: vec![],
        limit: 40,
    };
    let candidates = service::agent_candidates(&mut tx, &current.target, &search)
        .await?
        .items;
    if candidates.len() < 3 {
        sqlx::query("UPDATE question_generations SET state='insufficient_candidates',reasons=ARRAY['词库内符合条件的候选不足三个'],revision=revision+1,updated_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    } else {
        let target = current.target.0.clone();
        let correct_index = usize::from(id.as_bytes()[15]) % 4;
        let mut choices: Vec<_> = candidates.into_iter().take(3).collect();
        choices.insert(correct_index, target.clone());
        let options: Vec<_> = choices
            .into_iter()
            .enumerate()
            .map(|(index, candidate)| BankQuestionOption {
                option_id: ["A", "B", "C", "D"][index].into(),
                explanation: format!("{}：{}", candidate.spelling, candidate.definition),
                candidate,
            })
            .collect();
        let content = BankQuestionContent {
            prompt: target.definition.clone(),
            target,
            question_level: current.question_level.clone(),
            answer_profile: current.answer_profile.clone(),
            correct_option_id: ["A", "B", "C", "D"][correct_index].into(),
            explanation: options
                .iter()
                .map(|o| o.explanation.clone())
                .collect::<Vec<_>>()
                .join("\n"),
            options,
            concerns: vec!["算法仅筛选词库候选，请人工核查每个选项的语义和答案唯一性。".into()],
        };
        match source::validate_question(&mut tx, &content).await {
            Ok(()) => {
                let qid = Uuid::now_v7();
                sqlx::query("INSERT INTO question_bank(id,generation_id,content,original_content,target_entry_id,target_sense_id,target_publication_id,question_level,answer_profile,created_by,review_id,question_revision_id) VALUES($1,$2,$3,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
                    .bind(qid).bind(id).bind(Json(&content)).bind(content.target.source.entry_id).bind(content.target.source.sense_id).bind(content.target.source.publication_id).bind(&content.question_level).bind(&content.answer_profile).bind(current.created_by).bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(&mut *tx).await.map_err(AppError::internal)?;
                sqlx::query("UPDATE question_generations SET state='succeeded',question_id=$2,revision=revision+1,updated_at=now() WHERE id=$1").bind(id).bind(qid).execute(&mut *tx).await.map_err(AppError::internal)?;
            }
            Err(error) if !error.status_code().is_server_error() => {
                sqlx::query("UPDATE question_generations SET state='failed',error_code='source_or_question_invalid',revision=revision+1,updated_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
            }
            Err(error) => return Err(error),
        }
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(())
}
async fn process(state: &AppState, config: &AgentConfig, id: Uuid) -> Result<(), AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let row = service::generation(&mut tx, id).await?;
    let cancelled = ["cancelled", "failed"].contains(&row.state.as_str());
    if !cancelled && let Err(error) = service::delegation(&mut tx, id).await {
        if error.status_code().is_server_error() {
            return Err(error);
        }
        sqlx::query("UPDATE question_generations SET state='failed',error_code='authorization_or_source_changed',revision=revision+1,updated_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
        tx.commit().await.map_err(AppError::internal)?;
        return Ok(());
    }
    if !cancelled && row.agent_run_id.is_none() {
        sqlx::query(
            "UPDATE question_generations SET submitted_at=COALESCE(submitted_at,now()) WHERE id=$1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    }
    tx.commit().await.map_err(AppError::internal)?;
    if cancelled {
        let known_run = row.agent_run_id;
        if known_run.is_none() && row.submitted_at.is_some() {
            // The remote run identity is unknown. Do not replay creation: it could start a new AI run.
            return Err(super::conflict(
                "旧生成任务的远端执行身份未知，无法自动取消",
            ));
        }
        if let Some(run) = known_run {
            config
                .call(
                    reqwest::Method::POST,
                    &format!("/v1/runs/{run}/cancel"),
                    None,
                    None,
                )
                .await?;
        }
        sqlx::query("UPDATE question_generations SET cancel_delivered=true,agent_run_id=COALESCE(agent_run_id,$2) WHERE id=$1")
            .bind(id).bind(known_run)
            .execute(&state.pool)
            .await
            .map_err(AppError::internal)?;
        return Ok(());
    }
    let response = if let Some(run) = row.agent_run_id {
        config
            .call(reqwest::Method::GET, &format!("/v1/runs/{run}"), None, None)
            .await?
    } else {
        config.call(reqwest::Method::POST,"/v1/runs",Some(json!({"contract_version":1,"external_request_id":id,"capability":"vocabulary_mcq_zh_en"})),Some(id)).await?
    };
    let run: Uuid = serde_json::from_value(response["id"].clone()).map_err(AppError::internal)?;
    if response["external_request_id"] != json!(id)
        || response["capability"] != "vocabulary_mcq_zh_en"
        || response["contract_version"] != 1
    {
        return Err(super::invalid("Agent 执行身份不匹配"));
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let current = service::generation(&mut tx, id).await?;
    if current.state == "cancelled" {
        sqlx::query("UPDATE question_generations SET agent_run_id=$2,cancel_delivered=false,next_attempt_at=now() WHERE id=$1").bind(id).bind(run).execute(&mut *tx).await.map_err(AppError::internal)?;
        tx.commit().await.map_err(AppError::internal)?;
        return Ok(());
    }
    if !["queued", "running"].contains(&current.state.as_str()) {
        return Ok(());
    }
    if current.agent_run_id.is_some_and(|old| old != run) {
        return Err(super::conflict("Agent 执行身份冲突"));
    }
    service::delegation(&mut tx, id).await?;
    let remote = response["state"].as_str().unwrap_or("");
    let mut next = "running";
    let mut question_id = None;
    let mut reasons: Vec<String> = vec![];
    let mut error_code = response["error_code"].as_str().map(str::to_owned);
    match remote {
        "queued" | "running" => {}
        "failed" | "cancelled" => next = "failed",
        "succeeded" => {
            if response["result"]["outcome"] == "insufficient_candidates" {
                next = "insufficient_candidates";
                reasons = serde_json::from_value(response["result"]["reasons"].clone())
                    .map_err(AppError::internal)?;
            } else {
                let q = &response["result"]["question"];
                let content = import_question(q)?;
                let semantic_review: QuestionSemanticReview =
                    serde_json::from_value(q["semantic_review"].clone())
                        .map_err(|_| super::invalid("Agent 语义复审记录无效"))?;
                if (semantic_review.verdict == QuestionSemanticVerdict::NeedsRevision)
                    == semantic_review.issues.is_empty()
                {
                    return Err(super::invalid("Agent 语义复审结论与问题不一致"));
                }
                if content.target != current.target.0
                    || content.question_level != current.question_level
                    || content.answer_profile != current.answer_profile
                {
                    return Err(super::source_changed());
                }
                let validation = source::validate_question(&mut tx, &content).await;
                if let Err(error) = validation {
                    if error.status_code().is_server_error() {
                        return Err(error);
                    }
                    next = "failed";
                    error_code = Some("source_or_question_invalid".into());
                } else {
                    let qid = Uuid::now_v7();
                    sqlx::query("INSERT INTO question_bank(id,generation_id,content,original_content,target_entry_id,target_sense_id,target_publication_id,question_level,answer_profile,created_by,review_id,question_revision_id,semantic_review) VALUES($1,$2,$3,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)").bind(qid).bind(id).bind(Json(&content)).bind(content.target.source.entry_id).bind(content.target.source.sense_id).bind(content.target.source.publication_id).bind(&content.question_level).bind(&content.answer_profile).bind(current.created_by).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Json(&semantic_review)).execute(&mut *tx).await.map_err(AppError::internal)?;
                    question_id = Some(qid);
                    next = "succeeded";
                }
            }
        }
        _ => return Err(super::invalid("Agent 状态无效")),
    }
    sqlx::query("UPDATE question_generations SET agent_run_id=$2,state=$3,question_id=$4,error_code=$5,reasons=$6,revision=revision+1,updated_at=now(),next_attempt_at=now()+interval '3 seconds' WHERE id=$1").bind(id).bind(run).bind(next).bind(question_id).bind(error_code).bind(reasons).execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(())
}
fn import_question(q: &Value) -> Result<BankQuestionContent, AppError> {
    if q["requires_human_review"] != true {
        return Err(super::invalid("Agent 结果必须要求人工审核"));
    }
    let target: QuestionCandidate =
        serde_json::from_value(q["target"].clone()).map_err(AppError::internal)?;
    let mut options = vec![];
    for value in q["options"]
        .as_array()
        .ok_or_else(|| super::invalid("选项缺失"))?
    {
        let candidate: QuestionCandidate =
            serde_json::from_value(value["candidate"].clone()).map_err(AppError::internal)?;
        let explanation = if candidate == target {
            format!("{}：{}", candidate.spelling, candidate.definition)
        } else {
            let proposal = q["proposal"]["distractors"]
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find(|d| d["candidate_ref"] == candidate.candidate_ref)
                })
                .ok_or_else(|| super::invalid("干扰项理由缺失"))?;
            format!(
                "{}；{}",
                proposal["why_plausible"].as_str().unwrap_or(""),
                proposal["why_wrong"].as_str().unwrap_or("")
            )
        };
        options.push(BankQuestionOption {
            option_id: value["option_id"].as_str().unwrap_or("").into(),
            candidate,
            explanation,
        });
    }
    let explanation = options
        .iter()
        .map(|o| o.explanation.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let mut concerns: Vec<String> =
        serde_json::from_value(q["proposal"]["concerns"].clone()).map_err(AppError::internal)?;
    if let Some(prompt) = q["proposal"]["suggested_prompt"].as_str() {
        concerns.push(format!("建议题干：{prompt}"));
    }
    // Preserve human-readable warnings for clients using the legacy strict question response.
    if let Some(issues) = q["semantic_review"]["issues"].as_array() {
        concerns.extend(issues.iter().filter_map(|v| v.as_str().map(str::to_owned)));
    }
    Ok(BankQuestionContent {
        target,
        prompt: q["prompt"].as_str().unwrap_or("").into(),
        question_level: q["question_level"].as_str().unwrap_or("").into(),
        answer_profile: q["answer_profile"].as_str().unwrap_or("").into(),
        options,
        correct_option_id: q["correct_option_id"].as_str().unwrap_or("").into(),
        explanation,
        concerns,
    })
}
