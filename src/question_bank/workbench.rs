//! Bounded administrative reads; generation, editing and approval keep their authoritative checks.
use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, types::Json};
use uuid::Uuid;

use super::{dto::*, invalid, service, source};
use crate::{
    admin::AdminAuth, error::AppError, learning_tasks::repository::Tx,
    wordlists::service::page_meta,
};

#[derive(sqlx::FromRow)]
struct ListRow {
    id: Uuid,
    revision: i64,
    state: String,
    created_by: Uuid,
    content: Json<BankQuestionContent>,
    updated_at: DateTime<Utc>,
}

async fn current_fingerprints(
    tx: &mut Tx<'_>,
    contents: &[&BankQuestionContent],
) -> Result<HashSet<String>, AppError> {
    let mut entries: Vec<_> = contents
        .iter()
        .flat_map(|c| {
            c.options
                .iter()
                .map(|o| o.candidate.source.entry_id)
                .chain(std::iter::once(c.target.source.entry_id))
        })
        .collect();
    entries.sort();
    entries.dedup();
    let mut fingerprints = HashSet::new();
    // Decode only the sources of the returned page, shared between all its questions.
    for profile in ["BrE", "AmE"] {
        if contents.iter().any(|c| c.answer_profile == profile) {
            fingerprints.extend(
                source::candidates(tx, &entries, "C2", profile)
                    .await?
                    .into_iter()
                    .map(|c| c.content_fingerprint),
            );
        }
    }
    Ok(fingerprints)
}

fn validate_query(q: &QuestionWorkbenchQuery) -> Result<(), AppError> {
    if q.query.as_ref().is_some_and(|v| v.chars().count() > 100)
        || q.state
            .as_ref()
            .is_some_and(|s| !["draft", "approved", "rejected", "disabled"].contains(&s.as_str()))
    {
        return Err(invalid("题目检索参数无效"));
    }
    source::validate_context(
        q.question_level.as_deref().unwrap_or("C2"),
        q.answer_profile.as_deref().unwrap_or("BrE"),
    )
}

const QUESTION_FILTER: &str = " FROM question_bank WHERE ($1::text IS NULL OR content#>>'{target,spelling}' ILIKE $1 OR content->>'prompt' ILIKE $1 OR content#>>'{target,definition}' ILIKE $1) AND ($2::text IS NULL OR question_level=$2) AND ($3::text IS NULL OR answer_profile=$3)";
fn like_query(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|s| !s.is_empty()).map(|s| {
        format!(
            "%{}%",
            s.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    })
}

pub async fn list(
    pool: &PgPool,
    auth: &AdminAuth,
    q: QuestionWorkbenchQuery,
) -> Result<QuestionWorkbenchPage, AppError> {
    validate_query(&q)?;
    let (page, size) = service::pagination(&QuestionBankQuery {
        page: q.page,
        page_size: q.page_size,
        state: q.state.clone(),
    })?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::authorize(&mut tx, auth, "words.access", None).await?;
    let query = like_query(q.query.as_deref());
    let mut counts_sql = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT state,count(*)");
    counts_sql.push(QUESTION_FILTER).push(" GROUP BY state");
    let groups: Vec<(String, i64)> = counts_sql
        .build_query_as()
        .bind(&query)
        .bind(&q.question_level)
        .bind(&q.answer_profile)
        .fetch_all(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let mut counts = QuestionStateCounts::default();
    let mut total = 0;
    for (state, count) in groups {
        if q.state.as_ref().is_none_or(|s| *s == state) {
            total += count;
        }
        match state.as_str() {
            "draft" => counts.draft = count,
            "approved" => counts.approved = count,
            "rejected" => counts.rejected = count,
            "disabled" => counts.disabled = count,
            _ => {}
        }
    }
    let mut list_sql = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT id,revision,state,created_by,content,updated_at",
    );
    list_sql.push(QUESTION_FILTER).push(
        " AND ($4::text IS NULL OR state=$4) ORDER BY updated_at DESC,id DESC LIMIT $5 OFFSET $6",
    );
    let rows: Vec<ListRow> = list_sql
        .build_query_as()
        .bind(&query)
        .bind(&q.question_level)
        .bind(&q.answer_profile)
        .bind(&q.state)
        .bind(i64::from(size))
        .bind(i64::from(page - 1) * i64::from(size))
        .fetch_all(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let fingerprints = current_fingerprints(
        &mut tx,
        &rows.iter().map(|r| &r.content.0).collect::<Vec<_>>(),
    )
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            let c = row.content.0;
            let source_current = fingerprints.contains(&c.target.content_fingerprint)
                && c.options
                    .iter()
                    .all(|o| fingerprints.contains(&o.candidate.content_fingerprint));
            QuestionWorkbenchItem {
                id: row.id,
                revision: row.revision,
                state: row.state,
                created_by: row.created_by,
                spelling: c.target.spelling,
                part_of_speech: c.target.part_of_speech,
                definition: c.target.definition,
                prompt: c.prompt,
                question_level: c.question_level,
                answer_profile: c.answer_profile,
                options: c
                    .options
                    .into_iter()
                    .map(|o| QuestionWorkbenchOption {
                        option_id: o.option_id,
                        spelling: o.candidate.spelling,
                    })
                    .collect(),
                correct_option_id: c.correct_option_id,
                source_current,
                has_concerns: !c.concerns.is_empty(),
                updated_at: row.updated_at,
            }
        })
        .collect();
    tx.commit().await.map_err(AppError::internal)?;
    Ok(QuestionWorkbenchPage {
        items,
        pagination: page_meta(page, size, total),
        counts,
    })
}

pub async fn inspection(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
) -> Result<QuestionInspection, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::authorize(&mut tx, auth, "words.access", None).await?;
    let row = service::question(&mut tx, id).await?;
    let semantic_review = row.semantic_review.as_ref().map(|review| review.0.clone());
    let semantic_review_current = row.semantic_review_current();
    let question = row.view();
    let fingerprints = current_fingerprints(&mut tx, &[&question.content]).await?;
    let target_current = fingerprints.contains(&question.content.target.content_fingerprint);
    let sources = question
        .content
        .options
        .iter()
        .map(|o| QuestionSourceCheck {
            option_id: o.option_id.clone(),
            current: fingerprints.contains(&o.candidate.content_fingerprint),
        })
        .collect();
    tx.commit().await.map_err(AppError::internal)?;
    Ok(QuestionInspection {
        question,
        sources,
        target_current,
        semantic_review,
        semantic_review_current,
    })
}

#[derive(sqlx::FromRow)]
struct CandidateIdentity {
    entry_id: Uuid,
    publication_id: Uuid,
    pos_id: Uuid,
    sense_id: Uuid,
    definition_id: Uuid,
    form_id: Uuid,
}
impl CandidateIdentity {
    fn matches(&self, c: &QuestionCandidate) -> bool {
        let s = &c.source;
        s.entry_id == self.entry_id
            && s.publication_id == self.publication_id
            && s.pos_id == self.pos_id
            && s.sense_id == self.sense_id
            && s.definition_id == self.definition_id
            && s.form_id == self.form_id
    }
}
const CANDIDATE_SEARCH: &str = include_str!("candidate_search.sql");

pub async fn candidates(
    pool: &PgPool,
    auth: &AdminAuth,
    q: SearchQuestionCandidates,
) -> Result<QuestionCandidateSearchPage, AppError> {
    source::validate_context(&q.question_level, &q.answer_profile)?;
    if q.query.as_ref().is_some_and(|s| s.chars().count() > 100)
        || q.part_of_speech
            .as_ref()
            .is_some_and(|s| s.chars().count() > 100)
        || q.exclude_entry_ids.len() > 4
        || q.exclude_spellings.len() > 124
        || q.exclude_spellings.iter().any(|s| s.chars().count() > 200)
        || q.excluded_definition
            .as_ref()
            .is_some_and(|s| s.chars().count() > 2000)
        || q.target_spelling
            .as_ref()
            .is_some_and(|s| s.chars().count() > 200)
    {
        return Err(invalid("候选检索范围无效"));
    }
    let (page, size) = service::pagination(&QuestionBankQuery {
        page: q.page,
        page_size: q.page_size,
        state: None,
    })?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    let a = service::authorize(&mut tx, auth, "words.access", None).await?;
    if q.editable_only {
        a.require("words.edit")?;
    }
    let own_only = q.editable_only && !a.is_super_admin && !a.has("words.edit_others");
    let levels = ["A1", "A2", "B1", "B2", "C1", "C2"];
    let allowed = &levels[..=levels.iter().position(|l| *l == q.question_level).unwrap()];
    let query = like_query(q.query.as_deref());
    let excluded: Vec<_> = q
        .exclude_spellings
        .iter()
        .map(|s| crate::platform::text::key_v1(s))
        .collect();
    let definition = q
        .excluded_definition
        .as_deref()
        .map(crate::platform::text::key_v1);
    let mut count_sql = sqlx::QueryBuilder::<sqlx::Postgres>::new(CANDIDATE_SEARCH);
    count_sql.push(" SELECT count(*) FROM matches");
    let total: i64 = count_sql
        .build_query_scalar()
        .bind(q.entry_id)
        .bind(&query)
        .bind(allowed)
        .bind(&q.answer_profile)
        .bind(&q.part_of_speech)
        .bind(&q.exclude_entry_ids)
        .bind(&excluded)
        .bind(&definition)
        .bind(&q.target_spelling)
        .bind(own_only)
        .bind(a.admin_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let mut page_sql = sqlx::QueryBuilder::<sqlx::Postgres>::new(CANDIDATE_SEARCH);
    page_sql.push(" SELECT entry_id,publication_id,pos_id,sense_id,definition_id,form_id FROM matches ORDER BY relevance DESC,entry_id,pos_id,sense_id,definition_id,form_id LIMIT $12 OFFSET $13");
    let identities: Vec<CandidateIdentity> = page_sql
        .build_query_as()
        .bind(q.entry_id)
        .bind(&query)
        .bind(allowed)
        .bind(&q.answer_profile)
        .bind(&q.part_of_speech)
        .bind(&q.exclude_entry_ids)
        .bind(&excluded)
        .bind(&definition)
        .bind(&q.target_spelling)
        .bind(own_only)
        .bind(a.admin_id)
        .bind(i64::from(size))
        .bind(i64::from(page - 1) * i64::from(size))
        .fetch_all(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let ids: Vec<_> = identities.iter().map(|s| s.entry_id).collect();
    let fresh = source::candidates(&mut tx, &ids, &q.question_level, &q.answer_profile).await?;
    let mut items = Vec::with_capacity(identities.len());
    for identity in identities {
        // A concurrent republish must never silently bind an old identity to a new meaning.
        let candidate = fresh
            .iter()
            .find(|c| identity.matches(c))
            .ok_or_else(super::source_changed)?;
        if q.part_of_speech
            .as_ref()
            .is_some_and(|pos| *pos != candidate.part_of_speech)
            || q.exclude_entry_ids.contains(&candidate.source.entry_id)
            || candidate
                .accepted_spellings
                .iter()
                .chain(std::iter::once(&candidate.spelling))
                .any(|s| excluded.contains(&crate::platform::text::key_v1(s)))
            || definition.as_ref().is_some_and(|target| {
                candidate
                    .other_definitions
                    .iter()
                    .chain(std::iter::once(&candidate.definition))
                    .any(|d| crate::platform::text::key_v1(d) == *target)
            })
        {
            return Err(super::source_changed());
        }
        items.push(candidate.clone());
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(QuestionCandidateSearchPage {
        items,
        pagination: page_meta(page, size, total),
    })
}
