use super::{conflict, dto::*, invalid, source};
use crate::{
    admin::{AdminAuth, permissions},
    error::{AppError, ErrorCode},
    learning_tasks::repository::Tx,
    wordlists::service::{hash, page_meta},
};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, types::Json};
use uuid::Uuid;

#[derive(sqlx::FromRow)]
pub(super) struct GenerationRow {
    pub id: Uuid,
    pub created_by: Uuid,
    pub security_version: i64,
    pub permission_version: i64,
    pub target: Json<QuestionCandidate>,
    pub question_level: String,
    pub answer_profile: String,
    pub state: String,
    pub revision: i64,
    pub agent_run_id: Option<Uuid>,
    pub question_id: Option<Uuid>,
    pub error_code: Option<String>,
    pub reasons: Vec<String>,
    pub expires_at: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl GenerationRow {
    pub fn view(self) -> QuestionGeneration {
        QuestionGeneration {
            id: self.id,
            created_by: self.created_by,
            target: self.target.0,
            question_level: self.question_level,
            answer_profile: self.answer_profile,
            state: self.state,
            revision: self.revision,
            agent_run_id: self.agent_run_id,
            question_id: self.question_id,
            error_code: self.error_code,
            reasons: self.reasons,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
#[derive(sqlx::FromRow)]
pub(super) struct QuestionRow {
    pub id: Uuid,
    pub generation_id: Uuid,
    pub revision: i64,
    pub state: String,
    pub content: Json<BankQuestionContent>,
    pub original_content: Json<BankQuestionContent>,
    pub semantic_review: Option<Json<QuestionSemanticReview>>,
    pub created_by: Uuid,
    pub reviewed_by: Option<Uuid>,
    pub review_comment: String,
    pub review_id: Uuid,
    pub question_revision_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl QuestionRow {
    pub fn semantic_review_current(&self) -> bool {
        let current = &self.content.0;
        let original = &self.original_content.0;
        self.semantic_review.is_some()
            && current.prompt == original.prompt
            && current.target == original.target
            && current.options.len() == original.options.len()
            && current.options.iter().all(|option| {
                original
                    .options
                    .iter()
                    .any(|old| old.candidate == option.candidate)
            })
    }
    pub fn view(self) -> BankQuestion {
        BankQuestion {
            id: self.id,
            generation_id: self.generation_id,
            revision: self.revision,
            state: self.state,
            content: self.content.0,
            original_content: self.original_content.0,
            created_by: self.created_by,
            reviewed_by: self.reviewed_by,
            review_comment: self.review_comment,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
pub(super) async fn generation(tx: &mut Tx<'_>, id: Uuid) -> Result<GenerationRow, AppError> {
    sqlx::query_as("SELECT * FROM question_generations WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("生成委托不存在"))
}
pub(super) async fn question(tx: &mut Tx<'_>, id: Uuid) -> Result<QuestionRow, AppError> {
    sqlx::query_as("SELECT * FROM question_bank WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("题目不存在"))
}
pub async fn authorize(
    tx: &mut Tx<'_>,
    auth: &AdminAuth,
    key: &str,
    owner: Option<Uuid>,
) -> Result<permissions::AdminAuthorization, AppError> {
    permissions::lock_session(tx, auth, key).await?;
    let a = permissions::lock(tx, auth.subject).await?;
    if let Some(owner) = owner {
        a.require_owned_action("words.edit", owner)?;
    }
    Ok(a)
}
pub(super) async fn delegation(tx: &mut Tx<'_>, id: Uuid) -> Result<GenerationRow, AppError> {
    let row = generation(tx, id).await?;
    let auth = permissions::lock(tx, row.created_by).await?;
    let security: i64 = sqlx::query_scalar("SELECT security_version FROM admins WHERE id=$1")
        .bind(row.created_by)
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    if security != row.security_version
        || auth.permission_version != row.permission_version
        || !["queued", "running"].contains(&row.state.as_str())
        || (row.submitted_at.is_some() && row.expires_at <= Utc::now())
    {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "生成委托已过期、取消或权限已撤销",
        ));
    }
    let owner: Option<Uuid> =
        sqlx::query_scalar("SELECT created_by_admin_id FROM lexicon.entries WHERE id=$1")
            .bind(row.target.source.entry_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    auth.require("words.access")?;
    auth.require_owned_action("words.edit", owner.ok_or_else(super::source_changed)?)?;
    source::recheck(tx, &row.target).await?;
    Ok(row)
}
async fn candidate_entry_page(
    tx: &mut Tx<'_>,
    query: &QuestionCandidateQuery,
    after: Option<Uuid>,
) -> Result<Vec<Uuid>, AppError> {
    let levels = ["A1", "A2", "B1", "B2", "C1", "C2"];
    let rank = levels
        .iter()
        .position(|level| *level == query.question_level)
        .ok_or_else(|| invalid("级别无效"))?;
    let allowed_levels = levels[..=rank].to_vec();
    sqlx::query_scalar(
        r#"
        SELECT e.id FROM lexicon.entries e
        JOIN lexicon.entry_publications p ON p.id=e.current_publication_id
        WHERE e.archived_at IS NULL AND e.teaching_eligible AND p.content_schema_version=3
          AND ($1::uuid IS NULL OR e.id=$1)
          AND ($2::text IS NULL OR p.snapshot::text ILIKE '%'||$2||'%')
          AND ($3::uuid IS NULL OR e.id>$3)
          AND EXISTS (
            SELECT 1 FROM jsonb_array_elements(p.snapshot#>'{forms,pos}') f(value)
            CROSS JOIN LATERAL jsonb_array_elements(p.snapshot#>'{meanings,pos}') m(value)
            CROSS JOIN LATERAL jsonb_array_elements(m.value->'senses') s(value)
            CROSS JOIN LATERAL jsonb_array_elements(s.value->'definitions') d(value)
            WHERE f.value->>'pos_id'=m.value->>'pos_id'
              AND COALESCE((s.value->>'depends_on_context')::boolean,false)=false
              AND d.value->>'definition_mode'='zh_definition'
              AND d.value->>'level'=ANY($4)
          )
        ORDER BY e.id LIMIT 64
    "#,
    )
    .bind(query.entry_id)
    .bind(query.query.as_deref().filter(|s| !s.is_empty()))
    .bind(after)
    .bind(&allowed_levels)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::internal)
}

pub async fn candidates(
    tx: &mut Tx<'_>,
    query: &QuestionCandidateQuery,
) -> Result<QuestionCandidatePage, AppError> {
    source::validate_context(&query.question_level, &query.answer_profile)?;
    if query
        .query
        .as_ref()
        .is_some_and(|v| v.chars().count() > 100)
    {
        return Err(invalid("检索词过长"));
    }
    let mut after = None;
    let mut items = Vec::new();
    loop {
        let ids = candidate_entry_page(tx, query, after).await?;
        if ids.is_empty() {
            break;
        }
        after = ids.last().copied();
        items.extend(
            source::candidates(tx, &ids, &query.question_level, &query.answer_profile).await?,
        );
        if items.len() >= 80 || ids.len() < 64 {
            break;
        }
    }
    items.truncate(80);
    Ok(QuestionCandidatePage {
        contract_version: 1,
        items,
    })
}

pub async fn agent_candidates(
    tx: &mut Tx<'_>,
    target: &QuestionCandidate,
    input: &AgentCandidateSearch,
) -> Result<QuestionCandidatePage, AppError> {
    use crate::platform::text::key_v1;
    use std::collections::HashSet;
    source::validate_context(&input.question_level, &input.answer_profile)?;
    let mut items = Vec::new();
    let mut entries = HashSet::from([target.source.entry_id]);
    let mut spellings: HashSet<_> = target
        .accepted_spellings
        .iter()
        .chain(std::iter::once(&target.spelling))
        .map(|s| key_v1(s))
        .collect();
    let target_definition = key_v1(&target.definition);
    let ranked = ranked_candidate_entries(tx, target, input).await?;
    for ids in ranked.chunks(64) {
        let mut candidates =
            source::candidates(tx, ids, &input.question_level, &input.answer_profile).await?;
        // Authoritative reads use UUID lock order; restore retrieval order afterwards.
        candidates.sort_by_key(|c| ids.iter().position(|id| *id == c.source.entry_id));
        // A second request must move beyond entries already returned, not recycle another sense.
        for candidate in &candidates {
            if input
                .exclude_candidate_refs
                .contains(&candidate.candidate_ref)
            {
                entries.insert(candidate.source.entry_id);
                spellings.extend(
                    candidate
                        .accepted_spellings
                        .iter()
                        .chain(std::iter::once(&candidate.spelling))
                        .map(|s| key_v1(s)),
                );
            }
        }
        for candidate in candidates {
            if candidate.part_of_speech != target.part_of_speech
                || entries.contains(&candidate.source.entry_id)
                || candidate
                    .other_definitions
                    .iter()
                    .chain(std::iter::once(&candidate.definition))
                    .any(|d| key_v1(d) == target_definition)
            {
                continue;
            }
            let keys: HashSet<_> = candidate
                .accepted_spellings
                .iter()
                .chain(std::iter::once(&candidate.spelling))
                .map(|s| key_v1(s))
                .collect();
            if !keys.is_disjoint(&spellings) {
                continue;
            }
            entries.insert(candidate.source.entry_id);
            spellings.extend(keys);
            items.push(candidate);
            if items.len() == 40 {
                return Ok(QuestionCandidatePage {
                    contract_version: 1,
                    items,
                });
            }
        }
    }
    Ok(QuestionCandidatePage {
        contract_version: 1,
        items,
    })
}

async fn ranked_candidate_entries(
    tx: &mut Tx<'_>,
    target: &QuestionCandidate,
    input: &AgentCandidateSearch,
) -> Result<Vec<Uuid>, AppError> {
    let levels = ["A1", "A2", "B1", "B2", "C1", "C2"];
    let rank = levels
        .iter()
        .position(|level| *level == input.question_level)
        .unwrap();
    // Lexical similarity is only a recall signal, never evidence of a wrong answer.
    // Rank IDs once, then read authoritative snapshots in small batches until full.
    sqlx::query_scalar(
        "SELECT e.id FROM lexicon.question_candidate_index c JOIN lexicon.entries e ON e.id=c.entry_id AND e.current_publication_id=c.publication_id WHERE e.archived_at IS NULL AND e.teaching_eligible AND c.part_of_speech=$1 AND c.answer_profile=$4 AND c.level=ANY($5) GROUP BY e.id ORDER BY max(greatest(similarity(lower(c.spelling),lower($2)),similarity(c.definition,$3))) DESC,e.id",
    )
    .bind(&target.part_of_speech)
    .bind(&target.spelling)
    .bind(&target.definition)
    .bind(&input.answer_profile)
    .bind(&levels[..=rank])
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::internal)
}

pub async fn create(
    pool: &PgPool,
    auth: &AdminAuth,
    input: CreateQuestionGeneration,
) -> Result<QuestionGeneration, AppError> {
    source::validate_context(&input.question_level, &input.answer_profile)?;
    if input.answer_profile != input.target.answer_profile {
        return Err(invalid("目标档位不一致"));
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    authorize(&mut tx, auth, "words.edit", None).await?;
    super::batch::lock_creation(&mut tx).await?;
    let digest = hash(&input)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,2810))")
        .bind(format!("{}:{}", auth.subject, input.idempotency_key))
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let existing: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT id,request_hash FROM question_generations WHERE created_by=$1 AND request_key=$2",
    )
    .bind(auth.subject)
    .bind(input.idempotency_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    if let Some((id, old)) = existing {
        if old != digest {
            return Err(conflict("请求键已用于另一生成委托"));
        }
        let row = generation(&mut tx, id).await?;
        tx.commit().await.map_err(AppError::internal)?;
        return Ok(row.view());
    }
    let target = source::read(&mut tx, &input.target).await?;
    let owner: Uuid =
        sqlx::query_scalar("SELECT created_by_admin_id FROM lexicon.entries WHERE id=$1")
            .bind(target.source.entry_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let a = authorize(&mut tx, auth, "words.edit", Some(owner)).await?;
    if !source::candidates(
        &mut tx,
        &[target.source.entry_id],
        &input.question_level,
        &input.answer_profile,
    )
    .await?
    .contains(&target)
    {
        return Err(invalid("目标词义超过题目级别"));
    }
    let active: Option<Uuid> = sqlx::query_scalar("SELECT id FROM question_generations WHERE target#>>'{source,entry_id}'=$1 AND target#>>'{source,publication_id}'=$4 AND target#>>'{source,sense_id}'=$5 AND target#>>'{source,definition_id}'=$6 AND question_level=$2 AND answer_profile=$3 AND state IN ('queued','running') ORDER BY id LIMIT 1").bind(target.source.entry_id.to_string()).bind(&input.question_level).bind(&input.answer_profile).bind(target.source.publication_id.to_string()).bind(target.source.sense_id.to_string()).bind(target.source.definition_id.to_string()).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    if active.is_some() {
        return Err(conflict("当前词义、等级和档位已有生成任务，请查看生成记录"));
    }
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO question_generations(id,created_by,security_version,permission_version,request_key,request_hash,target,question_level,answer_profile) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(id).bind(auth.subject).bind(auth.security_version).bind(a.permission_version).bind(input.idempotency_key).bind(digest).bind(Json(target)).bind(input.question_level).bind(input.answer_profile).execute(&mut *tx).await.map_err(AppError::internal)?;
    let row = generation(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(row.view())
}
pub async fn cancel(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    input: CancelQuestionGeneration,
) -> Result<QuestionGeneration, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let row = generation(&mut tx, id).await?;
    authorize(&mut tx, auth, "words.edit", Some(row.created_by)).await?;
    if row.revision != input.expected_revision {
        return Err(conflict("生成委托版本已变化"));
    }
    if !["queued", "running", "cancelled"].contains(&row.state.as_str()) {
        return Err(conflict("生成已完成"));
    }
    sqlx::query("UPDATE question_generations SET state='cancelled',error_code=NULL,reasons='{}',cancel_delivered=(submitted_at IS NULL),revision=revision+1,updated_at=now(),next_attempt_at=now() WHERE id=$1 AND state<>'cancelled'").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    let row = generation(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(row.view())
}
pub(super) fn pagination(q: &QuestionBankQuery) -> Result<(u32, u32), AppError> {
    let p = q.page.unwrap_or(1);
    let s = q.page_size.unwrap_or(20);
    if p == 0 || !(1..=100).contains(&s) {
        return Err(invalid("分页无效"));
    }
    Ok((p, s))
}
pub async fn list_generations(
    pool: &PgPool,
    auth: &AdminAuth,
    q: QuestionBankQuery,
) -> Result<QuestionGenerationPage, AppError> {
    let (p, s) = pagination(&q)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    authorize(&mut tx, auth, "words.access", None).await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM question_generations WHERE ($1::text IS NULL OR state=$1)",
    )
    .bind(&q.state)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let rows:Vec<GenerationRow>=sqlx::query_as("SELECT * FROM question_generations WHERE ($1::text IS NULL OR state=$1) ORDER BY created_at DESC,id DESC LIMIT $2 OFFSET $3").bind(&q.state).bind(i64::from(s)).bind(i64::from(p-1)*i64::from(s)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(QuestionGenerationPage {
        items: rows.into_iter().map(GenerationRow::view).collect(),
        pagination: page_meta(p, s, total),
    })
}
pub async fn list_questions(
    pool: &PgPool,
    auth: &AdminAuth,
    q: QuestionBankQuery,
) -> Result<BankQuestionPage, AppError> {
    let (p, s) = pagination(&q)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    authorize(&mut tx, auth, "words.access", None).await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM question_bank WHERE ($1::text IS NULL OR state=$1)",
    )
    .bind(&q.state)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let rows:Vec<QuestionRow>=sqlx::query_as("SELECT * FROM question_bank WHERE ($1::text IS NULL OR state=$1) ORDER BY updated_at DESC,id DESC LIMIT $2 OFFSET $3").bind(&q.state).bind(i64::from(s)).bind(i64::from(p-1)*i64::from(s)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(BankQuestionPage {
        items: rows.into_iter().map(QuestionRow::view).collect(),
        pagination: page_meta(p, s, total),
    })
}
pub async fn edit(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    input: EditBankQuestion,
) -> Result<BankQuestion, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let row = question(&mut tx, id).await?;
    authorize(&mut tx, auth, "words.edit", Some(row.created_by)).await?;
    if row.revision != input.expected_revision
        || !["draft", "rejected", "disabled"].contains(&row.state.as_str())
    {
        return Err(conflict("版本已变化或题目已启用，请刷新"));
    }
    if input.options.len() != 4 || input.option_explanations.len() != 4 {
        return Err(invalid("需要四个选项及解释"));
    }
    let mut content = row.content.0.clone();
    content.prompt = input.prompt;
    content.explanation = input.explanation;
    content.options.clear();
    content.correct_option_id.clear();
    for (i, item) in input.options.iter().enumerate() {
        let candidate = source::read(&mut tx, item).await?;
        let option_id = ["A", "B", "C", "D"][i].to_owned();
        if candidate == content.target {
            content.correct_option_id = option_id.clone();
        }
        content.options.push(BankQuestionOption {
            option_id,
            candidate,
            explanation: input.option_explanations[i].clone(),
        });
    }
    source::validate_question(&mut tx, &content).await?;
    sqlx::query("UPDATE question_bank SET content=$2,state='draft',revision=revision+1,review_id=$3,question_revision_id=$4,updated_at=now() WHERE id=$1").bind(id).bind(Json(content)).bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(&mut *tx).await.map_err(AppError::internal)?;
    let row = question(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(row.view())
}
pub async fn review(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    input: ReviewBankQuestion,
) -> Result<BankQuestion, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let row = question(&mut tx, id).await?;
    authorize(&mut tx, auth, "words.publish", Some(row.created_by)).await?;
    if row.revision != input.expected_revision {
        return Err(conflict("审核版本冲突，请刷新"));
    }
    if input.comment.chars().count() > 2000
        || input.reason_codes.len() > 7
        || input.reason_codes.iter().any(|r| {
            ![
                "ambiguous",
                "weak_distractor",
                "level_mismatch",
                "source_error",
                "prompt_unclear",
                "explanation_error",
                "other",
            ]
            .contains(&r.as_str())
        })
    {
        return Err(invalid("审核理由无效"));
    }
    let state = match input.decision.as_str() {
        "approve" if ["draft", "rejected", "disabled"].contains(&row.state.as_str()) => "approved",
        "reject"
            if ["draft", "rejected"].contains(&row.state.as_str())
                && !input.reason_codes.is_empty() =>
        {
            "rejected"
        }
        "disable" if row.state == "approved" => "disabled",
        _ => return Err(conflict("当前状态不允许此审核操作")),
    };
    if state == "approved" {
        let method: String =
            sqlx::query_scalar("SELECT generation_method FROM question_generations WHERE id=$1")
                .bind(row.generation_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(AppError::internal)?;
        if method != "algorithm" {
            return Err(conflict("AI 生成的题目已停用，请使用算法重新命题"));
        }
        if row.semantic_review_current()
            && row
                .semantic_review
                .as_ref()
                .is_some_and(|review| review.verdict == QuestionSemanticVerdict::NeedsRevision)
        {
            return Err(conflict(
                "当前题面仍存在未解决的歧义，请更换干扰项或重新命题",
            ));
        }
        source::validate_question(&mut tx, &row.content).await?;
    }
    let run: Option<Uuid> =
        sqlx::query_scalar("SELECT agent_run_id FROM question_generations WHERE id=$1")
            .bind(row.generation_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    if let Some(run) = run {
        let event_id = Uuid::now_v7();
        let decision = match state {
            "approved" => "edited_and_approved",
            "rejected" => "rejected",
            _ => "revoked",
        };
        let lesson = if state == "rejected" {
            if input.comment.trim().is_empty() {
                input.reason_codes.join("; ")
            } else {
                input.comment.trim().to_owned()
            }
        } else {
            row.content.explanation.clone()
        };
        let example = if state == "approved" || state == "rejected" {
            Some(
                serde_json::json!({"target":row.content.target,"question_level":row.content.question_level,"prompt":row.content.prompt,"distractors":row.content.options.iter().filter(|o|o.option_id!=row.content.correct_option_id).map(|o|&o.candidate).collect::<Vec<_>>(),"lesson":lesson}),
            )
        } else {
            None
        };
        let payload = serde_json::json!({"contract_version":1,"event_id":event_id,"review_id":row.review_id,"review_revision":row.revision+1,"source_run_id":run,"question_revision_id":row.question_revision_id,"capability":"vocabulary_mcq_zh_en","decision":decision,"reason_codes":input.reason_codes,"comment":input.comment,"example":example});
        sqlx::query(
            "INSERT INTO question_review_deliveries(event_id,question_id,payload) VALUES($1,$2,$3)",
        )
        .bind(event_id)
        .bind(id)
        .bind(payload)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    }
    sqlx::query("UPDATE question_bank SET state=$2,revision=revision+1,reviewed_by=$3,review_comment=$4,updated_at=now() WHERE id=$1").bind(id).bind(state).bind(auth.subject).bind(input.comment).execute(&mut *tx).await.map_err(AppError::internal)?;
    let row = question(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(row.view())
}
