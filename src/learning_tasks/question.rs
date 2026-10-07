//! Versioned, deterministic publication-to-question projection. Never a public answer DTO.
use super::dto::{LearningContext, LearningExclusions, LearningPrompt};
use crate::{
    error::{AppError, ErrorCode},
    lexicon::{
        dto::*,
        normalization::{HEADWORD_NORMALIZATION_VERSION, normalize_headword},
        service::form_senses::allowed_form_senses,
    },
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;
pub const GENERATION_VERSION: &str = "zh_base_v1";
pub const GRADING_VERSION: &str = "spelling_exact_v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerSnapshot {
    pub answers: Vec<String>,
    pub keys: Vec<String>,
    pub normalization_version: i16,
}
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub source_wordlist_id: Uuid,
    pub source_revision: i64,
    pub entry_id: Uuid,
    pub entry_archive_generation: i64,
    pub publication_id: Uuid,
    pub pos_id: Uuid,
    pub sense_id: Uuid,
    pub definition_id: Uuid,
    pub form_ids: Vec<Uuid>,
    pub unit_key: String,
    pub prompt: LearningPrompt,
    pub answer: AnswerSnapshot,
}
pub struct LearningSource {
    pub wordlist_id: Uuid,
    pub revision: i64,
    pub entry_id: Uuid,
    pub archive_generation: i64,
    pub publication_id: Uuid,
    pub word: AdminWordV3,
}
pub struct CandidatePool {
    pub candidates: Vec<Candidate>,
    pub exclusions: LearningExclusions,
}
fn rank(level: &str) -> Option<usize> {
    ["A1", "A2", "B1", "B2", "C1", "C2"]
        .iter()
        .position(|l| *l == level)
}
fn definition_level(d: &WordDefinitionV3) -> &str {
    match d {
        WordDefinitionV3::ZhDefinition { level, .. }
        | WordDefinitionV3::ZhSentence { level, .. }
        | WordDefinitionV3::EnDefinition { level, .. }
        | WordDefinitionV3::EnSentence { level, .. } => level,
    }
}
fn chinese(d: &WordDefinitionV3) -> bool {
    matches!(
        d,
        WordDefinitionV3::ZhDefinition { .. } | WordDefinitionV3::ZhSentence { .. }
    )
}
fn select_definition<'a>(
    definitions: &'a [WordDefinitionV3],
    level: &str,
) -> Option<&'a WordDefinitionV3> {
    let max = rank(level)?;
    let best = definitions
        .iter()
        .filter_map(|d| rank(definition_level(d)))
        .filter(|r| *r <= max)
        .max()?;
    definitions
        .iter()
        .find(|d| rank(definition_level(d)) == Some(best) && chinese(d))
        .or_else(|| {
            definitions
                .iter()
                .find(|d| rank(definition_level(d)) == Some(best))
        })
}
fn leaks(prompt: &str, key: &str) -> bool {
    // Latin word boundaries allow “cat” in “category” but reject “猫 cat / cat 猫”.
    let text = crate::lexicon::normalization::normalize_text_key(prompt);
    static WORD_CHAR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[\p{Script=Latin}\p{Mark}\p{Number}]$").expect("Latin word boundary")
    });
    let word_char = |c: char| WORD_CHAR.is_match(&c.to_string());
    text.match_indices(key).any(|(i, _)| {
        text[..i].chars().next_back().is_none_or(|c| !word_char(c))
            && text[i + key.len()..]
                .chars()
                .next()
                .is_none_or(|c| !word_char(c))
    })
}
pub fn build_candidates(
    sources: &[LearningSource],
    context: &LearningContext,
) -> Result<CandidatePool, AppError> {
    let mut pool = CandidatePool {
        candidates: vec![],
        exclusions: LearningExclusions::default(),
    };
    let mut seen = HashSet::new();
    for source in sources {
        if source.word.id != source.entry_id {
            return Err(AppError::internal(std::io::Error::other(
                "publication entry mismatch",
            )));
        }
        for pos in &source.word.forms.pos {
            let Some(meanings) = source
                .word
                .meanings
                .pos
                .iter()
                .find(|m| m.pos_id == pos.pos_id)
            else {
                continue;
            };
            for sense in &meanings.senses {
                let unit_key = format!("{}:{}:{}", source.entry_id, pos.pos_id, sense.id);
                if !seen.insert(unit_key.clone()) {
                    continue;
                }
                if sense.depends_on_context {
                    pool.exclusions.context_dependent += 1;
                    continue;
                }
                let Some(WordDefinitionV3::ZhDefinition { id, content, .. }) =
                    select_definition(&sense.definitions, &context.cefr_level)
                else {
                    pool.exclusions.no_chinese_definition += 1;
                    continue;
                };
                let text = content.text().trim();
                if text.is_empty() {
                    pool.exclusions.no_chinese_definition += 1;
                    continue;
                }
                let mut answer = AnswerSnapshot {
                    answers: vec![],
                    keys: vec![],
                    normalization_version: HEADWORD_NORMALIZATION_VERSION,
                };
                let mut form_ids = vec![];
                for form in &pos.forms {
                    if form.form_type.as_str() != "base"
                        || !allowed_form_senses(pos, &source.word.meanings, form.id)
                            .any(|s| s.id == sense.id)
                    {
                        continue;
                    }
                    let spelling = match &form.regional_variants {
                        WordRegionalVariantsV3::Common { common } => &common.spelling,
                        WordRegionalVariantsV3::UkUs { uk, us } => {
                            if context.english_variant == "BrE" {
                                &uk.spelling
                            } else {
                                &us.spelling
                            }
                        }
                    };
                    if let Ok(n) = normalize_headword(spelling) {
                        form_ids.push(form.id);
                        if !answer.keys.contains(&n.key) {
                            answer.answers.push(n.display);
                            answer.keys.push(n.key);
                        }
                    }
                }
                if answer.keys.is_empty() {
                    pool.exclusions.no_base_form += 1;
                    continue;
                }
                if answer.keys.iter().any(|key| leaks(text, key)) {
                    pool.exclusions.answer_in_prompt += 1;
                    continue;
                }
                pool.candidates.push(Candidate {
                    source_wordlist_id: source.wordlist_id,
                    source_revision: source.revision,
                    entry_id: source.entry_id,
                    entry_archive_generation: source.archive_generation,
                    publication_id: source.publication_id,
                    pos_id: pos.pos_id,
                    sense_id: sense.id,
                    definition_id: *id,
                    form_ids,
                    unit_key,
                    prompt: LearningPrompt {
                        definition: text.to_owned(),
                        part_of_speech: pos.pos.clone(),
                    },
                    answer,
                });
            }
        }
    }
    Ok(pool)
}
pub fn grade_spelling(snapshot: &AnswerSnapshot, answer: &str) -> Result<(String, bool), AppError> {
    if snapshot.normalization_version != HEADWORD_NORMALIZATION_VERSION {
        return Err(AppError::conflict(
            ErrorCode::LearningRunUnavailable,
            None,
            "判定版本不支持",
        ));
    }
    // Bound raw input as well as normalized text to avoid unbounded whitespace submissions.
    if answer.chars().count() > 400 {
        return Err(AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "答案过长",
        ));
    }
    let n = normalize_headword(answer).map_err(|_| {
        AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "答案不能为空、包含控制字符或超过 200 字",
        )
    })?;
    let correct = snapshot.keys.contains(&n.key);
    Ok((n.key, correct))
}
pub fn business_window(at: DateTime<Utc>) -> (NaiveDate, DateTime<Utc>, DateTime<Utc>) {
    // Shanghai has fixed UTC+08:00 for all supported learning dates; 04:00 is 20:00 UTC.
    let day = (at + Duration::hours(4)).date_naive();
    let start = day.and_hms_opt(0, 0, 0).expect("midnight").and_utc() - Duration::hours(4);
    (day, start, start + Duration::days(1))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundary_and_exact_grading() {
        let before = "2026-10-07T19:59:59Z".parse().unwrap();
        let after = "2026-10-07T20:00:00Z".parse().unwrap();
        assert_eq!(business_window(before).0.to_string(), "2026-10-07");
        assert_eq!(business_window(after).0.to_string(), "2026-10-08");
        let snapshot = AnswerSnapshot {
            answers: vec!["don't".into()],
            keys: vec!["don't".into()],
            normalization_version: 1,
        };
        assert!(grade_spelling(&snapshot, "  ＤＯＮ’Ｔ  ").unwrap().1);
        assert!(!grade_spelling(&snapshot, "完全错误").unwrap().1);
        assert!(grade_spelling(&snapshot, "  ").is_err());
        assert!(grade_spelling(&snapshot, "a\nb").is_err());
        assert!(leaks("猫 cat", "cat"));
        assert!(leaks("cat×示例", "cat"));
        assert!(leaks(&format!("{} ＣＡＴ", "中".repeat(201)), "cat"));
        assert!(leaks("换行\nＤＯＮ’Ｔ", "don't"));
        assert!(!leaks("类别 category", "cat"));
    }
}
