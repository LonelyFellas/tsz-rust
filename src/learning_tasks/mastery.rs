//! Bounded practice within a plan. First-answer facts remain separate from retries.
use super::{question::business_window, repository::*};
use crate::error::AppError;
use chrono::{Duration, NaiveDate};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

pub const MODE: &str = "mastery_v1";
pub const UNKNOWN: &str = "不确定";
pub fn capable(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-learning-capabilities")
        .and_then(|s| s.to_str().ok())
        .is_some_and(|s| s.split(',').any(|s| s.trim() == "mastery-v1"))
}
pub fn require(headers: &axum::http::HeaderMap, mode: Option<&str>) -> Result<(), AppError> {
    if mode.is_some_and(|m| m != MODE) {
        return Err(invalid("练习模式无效"));
    }
    if mode.is_some() && !capable(headers) {
        return Err(conflict("请更新客户端后使用按掌握情况练习"));
    }
    Ok(())
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct MasterySummary {
    pub new_count: i64,
    pub learning_count: i64,
    pub mastered_count: i64,
    pub due_count: i64,
    #[schema(required = true)]
    pub next_review_day: Option<NaiveDate>,
}
pub async fn summary(
    tx: &mut Tx<'_>,
    cycle: Uuid,
    day: NaiveDate,
) -> Result<MasterySummary, AppError> {
    sqlx::query_as("SELECT count(*) FILTER(WHERE covered_answer_id IS NULL) AS new_count,count(*) FILTER(WHERE covered_answer_id IS NOT NULL AND practice_success_days<2) AS learning_count,count(*) FILTER(WHERE practice_success_days=2) AS mastered_count,count(*) FILTER(WHERE practice_next_day<=$2 AND practice_last_day<$2) AS due_count,min(practice_next_day) FILTER(WHERE practice_next_day>$2) AS next_review_day FROM learning_cycle_units WHERE cycle_id=$1")
        .bind(cycle).bind(day).fetch_one(&mut **tx).await.map_err(AppError::internal)
}
pub async fn eligible(tx: &mut Tx<'_>, cycle: Uuid, day: NaiveDate) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar("SELECT id FROM learning_cycle_units WHERE cycle_id=$1 AND (covered_answer_id IS NULL OR (practice_next_day<=$2 AND practice_last_day<$2)) ORDER BY (covered_answer_id IS NULL),practice_next_day NULLS LAST,position")
        .bind(cycle).bind(day).fetch_all(&mut **tx).await.map_err(AppError::internal)
}
/// Same-day retries can recover familiarity, but never establish cross-day confirmation.
fn advance(
    days: i16,
    last: Option<NaiveDate>,
    step: i16,
    correct: bool,
    day: NaiveDate,
) -> (i16, Option<NaiveDate>, i16, NaiveDate) {
    if !correct {
        return (0, None, 0, day + Duration::days(1));
    }
    let days = if last.is_some_and(|d| d >= day) {
        days.max(1)
    } else {
        (days + 1).min(2)
    };
    let step = if days == 2 && last.is_some_and(|d| d < day) {
        (step + 1).min(4)
    } else {
        step
    };
    let interval = [1, 3, 7, 14, 30][step as usize];
    (days, Some(day), step, day + Duration::days(interval))
}
pub async fn record(tx: &mut Tx<'_>, q: &QuestionRow, correct: bool) -> Result<(), AppError> {
    let unit = q
        .cycle_unit_id
        .ok_or_else(|| conflict("练习缺少学习目标"))?;
    let day = business_window(now(tx).await?).0;
    let (days,last,step):(i16,Option<NaiveDate>,i16)=sqlx::query_as("SELECT practice_success_days,practice_last_success_day,practice_interval_step FROM learning_cycle_units WHERE id=$1 FOR UPDATE").bind(unit).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
    let (days, last, step, due) = advance(days, last, step, correct, day);
    sqlx::query("UPDATE learning_cycle_units SET practice_success_days=$2,practice_last_success_day=$3,practice_interval_step=$4,practice_next_day=$5,practice_last_day=$6 WHERE id=$1").bind(unit).bind(days).bind(last).bind(step).bind(due).bind(day).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
pub async fn retry(
    tx: &mut Tx<'_>,
    row: &RunRow,
    q: &QuestionRow,
    remaining: usize,
) -> Result<bool, AppError> {
    if remaining < 3 || row.target_count >= 30 {
        return Ok(false);
    }
    let mut answer: super::question::AnswerSnapshot =
        serde_json::from_value(q.answer_snapshot.clone()).map_err(AppError::internal)?;
    crate::question_bank::learning::shuffle_answer(&mut answer);
    sqlx::query("INSERT INTO learning_questions(id,run_id,cycle_id,cycle_unit_id,position,source_wordlist_id,source_revision,source_membership_id,source_public_generation,entry_id,entry_archive_generation,publication_id,pos_id,sense_id,definition_id,form_ids,unit_key,prompt_snapshot,answer_snapshot,fingerprint,custom_item_id,practice_kind,retry_of) SELECT $2,run_id,cycle_id,cycle_unit_id,$3,source_wordlist_id,source_revision,source_membership_id,source_public_generation,entry_id,entry_archive_generation,publication_id,pos_id,sense_id,definition_id,form_ids,unit_key,prompt_snapshot,$4,fingerprint,custom_item_id,'retry',id FROM learning_questions WHERE id=$1")
        .bind(q.id).bind(Uuid::now_v7()).bind(row.target_count).bind(serde_json::to_value(answer).map_err(AppError::internal)?).execute(&mut **tx).await.map_err(AppError::internal)?;
    sqlx::query("UPDATE learning_runs SET target_count=target_count+1 WHERE id=$1")
        .bind(row.id)
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_requires_distinct_days_and_failure_resets_it() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
        let a = advance(0, None, 0, true, day);
        assert_eq!((a.0, a.2, a.3), (1, 0, day + Duration::days(1)));
        let b = advance(a.0, a.1, a.2, true, day);
        assert_eq!((b.0, b.2, b.3), (1, 0, day + Duration::days(1)));
        let c = advance(b.0, b.1, b.2, true, day + Duration::days(1));
        assert_eq!((c.0, c.2, c.3), (2, 1, day + Duration::days(4)));
        let d = advance(c.0, c.1, c.2, true, day + Duration::days(4));
        assert_eq!((d.0, d.2, d.3), (2, 2, day + Duration::days(11)));
        assert_eq!(
            advance(d.0, d.1, d.2, false, day + Duration::days(11)),
            (0, None, 0, day + Duration::days(12))
        );
    }
}
