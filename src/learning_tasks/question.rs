//! Versioned, deterministic publication-to-question projection. Never a public answer DTO.
use super::dto::{LearningContext, LearningExclusions, LearningPrompt};
use crate::{
    error::{AppError, ErrorCode},
    lexicon::{
        dto::{WordDefinitionV3, WordRegionalVariantsV3},
        published::{PublishedContentV3, allowed_form_senses},
    },
    platform::text::{display_v1, key_v1},
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;
pub const GENERATION_VERSION: &str = "zh_base_v1";
pub const WORDLIST_GRADING_VERSION: &str = "wordlist_mixed_v1";
pub const WORDLIST_GENERATION_VERSION: &str = "wordlist_all_v1";
pub const GRADING_VERSION: &str = "spelling_exact_v1";
pub const ANSWER_NORMALIZATION_VERSION: i16 = 1;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_word: Option<crate::wordlists::dto::CustomWord>,
    #[serde(default)]
    pub wordlist_fallback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<crate::question_bank::learning::ChoiceSnapshot>,
    pub answers: Vec<String>,
    pub keys: Vec<String>,
    pub normalization_version: i16,
}
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub custom_item_id: Option<Uuid>,
    pub source_wordlist_id: Uuid,
    pub source_revision: i64,
    pub source_membership_id: Uuid,
    pub source_public_generation: Option<i64>,
    pub entry_id: Option<Uuid>,
    pub entry_archive_generation: Option<i64>,
    pub publication_id: Option<Uuid>,
    pub pos_id: Option<Uuid>,
    pub sense_id: Option<Uuid>,
    pub definition_id: Option<Uuid>,
    pub form_ids: Vec<Uuid>,
    pub unit_key: String,
    pub prompt: LearningPrompt,
    pub answer: AnswerSnapshot,
}
pub struct LearningSource {
    pub wordlist_id: Uuid,
    pub revision: i64,
    pub membership_id: Uuid,
    pub public_generation: Option<i64>,
    pub entry_id: Uuid,
    pub archive_generation: i64,
    pub publication_id: Uuid,
    pub content: PublishedContentV3,
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
pub(crate) fn leaks(prompt: &str, key: &str) -> bool {
    // Latin word boundaries allow “cat” in “category” but reject “猫 cat / cat 猫”.
    let text = key_v1(prompt);
    static WORD_CHAR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[\p{Script=Latin}\p{Mark}\p{Number}]$").expect("Latin word boundary")
    });
    let word_char = |c: char| {
        let mut buffer = [0; 4];
        WORD_CHAR.is_match(c.encode_utf8(&mut buffer))
    };
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
    build_with_policy(sources, context, false)
}

pub fn build_wordlist_candidates(
    sources: &[LearningSource],
    context: &LearningContext,
) -> Result<CandidatePool, AppError> {
    build_with_policy(sources, context, true)
}

fn definition_text<'a>(
    definition: &'a WordDefinitionV3,
    context: &LearningContext,
) -> Option<(Uuid, &'a str)> {
    use crate::lexicon::dto::{DialectVariantRichTextSlotV3, EnglishTextV3};
    let (id, text) = match definition {
        WordDefinitionV3::ZhDefinition { id, content, .. }
        | WordDefinitionV3::ZhSentence { id, content, .. } => (*id, content.text()),
        WordDefinitionV3::EnDefinition { id, content, .. }
        | WordDefinitionV3::EnSentence { id, content, .. } => {
            let value = match content {
                EnglishTextV3::Unified { common } => &common.value,
                EnglishTextV3::Distinguish { uk, us, .. } => {
                    let slots = if context.english_variant == "BrE" {
                        [uk, us]
                    } else {
                        [us, uk]
                    };
                    slots.into_iter().find_map(|slot| match slot {
                        DialectVariantRichTextSlotV3::Ready { variant }
                            if !variant.value.text().trim().is_empty() =>
                        {
                            Some(&variant.value)
                        }
                        _ => None,
                    })?
                }
            };
            (*id, value.text())
        }
    };
    let text = text.trim();
    (!text.is_empty()).then_some((id, text))
}

fn build_with_policy(
    sources: &[LearningSource],
    context: &LearningContext,
    all_content: bool,
) -> Result<CandidatePool, AppError> {
    let mut pool = CandidatePool {
        candidates: vec![],
        exclusions: LearningExclusions::default(),
    };
    let mut seen = HashSet::new();
    for source in sources {
        for pos in &source.content.forms.pos {
            let Some(meanings) = source
                .content
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
                if sense.depends_on_context && !all_content {
                    pool.exclusions.context_dependent += 1;
                    continue;
                }
                let standard_definition =
                    select_definition(&sense.definitions, &context.cefr_level)
                        .filter(|d| matches!(d, WordDefinitionV3::ZhDefinition { .. }))
                        .and_then(|d| definition_text(d, context));
                let selected = standard_definition.or_else(|| {
                    if !all_content {
                        return None;
                    }
                    sense
                        .definitions
                        .iter()
                        .filter(|d| chinese(d))
                        .find_map(|d| definition_text(d, context))
                        .or_else(|| {
                            sense
                                .definitions
                                .iter()
                                .find_map(|d| definition_text(d, context))
                        })
                });
                let Some((definition_id, text)) = selected else {
                    pool.exclusions.no_chinese_definition += 1;
                    continue;
                };
                let forms: Vec<_> = pos
                    .forms
                    .iter()
                    .filter(|form| {
                        allowed_form_senses(pos, &source.content.meanings, form.id)
                            .any(|s| s.id == sense.id)
                    })
                    .collect();
                let has_base = forms.iter().any(|f| f.form_type.as_str() == "base");
                let mut answer = AnswerSnapshot {
                    custom_word: None,
                    wordlist_fallback: false,
                    choice: None,
                    answers: vec![],
                    keys: vec![],
                    normalization_version: ANSWER_NORMALIZATION_VERSION,
                };
                let mut form_ids = vec![];
                for form in forms
                    .into_iter()
                    .filter(|f| (all_content && !has_base) || f.form_type.as_str() == "base")
                {
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
                    if let Some((display, key)) = normalize_answer_v1(spelling) {
                        form_ids.push(form.id);
                        if !answer.keys.contains(&key) {
                            answer.answers.push(display);
                            answer.keys.push(key);
                        }
                    }
                }
                if answer.keys.is_empty() {
                    pool.exclusions.no_base_form += 1;
                    continue;
                }
                let contains_answer = answer.keys.iter().any(|key| leaks(text, key));
                if contains_answer && !all_content {
                    pool.exclusions.answer_in_prompt += 1;
                    continue;
                }
                answer.wordlist_fallback = sense.depends_on_context
                    || standard_definition.is_none()
                    || !has_base
                    || contains_answer;
                pool.candidates.push(Candidate {
                    custom_item_id: None,
                    source_wordlist_id: source.wordlist_id,
                    source_revision: source.revision,
                    source_membership_id: source.membership_id,
                    source_public_generation: source.public_generation,
                    entry_id: Some(source.entry_id),
                    entry_archive_generation: Some(source.archive_generation),
                    publication_id: Some(source.publication_id),
                    pos_id: Some(pos.pos_id),
                    sense_id: Some(sense.id),
                    definition_id: Some(definition_id),
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

pub fn custom_candidate(
    source: &super::repository::CustomSource,
    revision: i64,
    generation: Option<i64>,
) -> Option<Candidate> {
    let word = &source.custom_word.0;
    let (answer, key) = normalize_answer_v1(&display_v1(&word.word))?;
    Some(Candidate {
        custom_item_id: Some(source.item_id),
        source_wordlist_id: source.wordlist_id,
        source_revision: revision,
        source_membership_id: source.learning_membership_id,
        source_public_generation: generation,
        entry_id: None,
        entry_archive_generation: None,
        publication_id: None,
        pos_id: None,
        sense_id: None,
        definition_id: None,
        form_ids: vec![],
        unit_key: format!("custom:{}:{}", source.wordlist_id, source.item_id),
        prompt: LearningPrompt {
            definition: word.definition.clone(),
            part_of_speech: String::new(),
        },
        answer: AnswerSnapshot {
            custom_word: Some(word.clone()),
            wordlist_fallback: true,
            choice: None,
            answers: vec![answer],
            keys: vec![key],
            normalization_version: ANSWER_NORMALIZATION_VERSION,
        },
    })
}

fn normalize_answer_v1(value: &str) -> Option<(String, String)> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let display = display_v1(value);
    if display.is_empty() || display.chars().count() > 200 {
        return None;
    }
    let key = key_v1(&display);
    Some((display, key))
}

pub fn grade_spelling(snapshot: &AnswerSnapshot, answer: &str) -> Result<(String, bool), AppError> {
    let normalize = match snapshot.normalization_version {
        1 => normalize_answer_v1,
        _ => {
            return Err(AppError::conflict(
                ErrorCode::LearningRunUnavailable,
                None,
                "判定版本不支持",
            ));
        }
    };
    // Bound raw input as well as normalized text to avoid unbounded whitespace submissions.
    if answer.chars().count() > 400 {
        return Err(AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "答案过长",
        ));
    }
    let (_, key) = normalize(answer).ok_or_else(|| {
        AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "答案不能为空、包含控制字符或超过 200 字",
        )
    })?;
    let correct = snapshot.keys.contains(&key);
    Ok((key, correct))
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
            custom_word: None,
            wordlist_fallback: false,
            choice: None,
            answers: vec!["don't".into()],
            keys: vec!["don't".into()],
            normalization_version: 1,
        };
        assert!(grade_spelling(&snapshot, "  ＤＯＮ’Ｔ  ").unwrap().1);
        assert!(!grade_spelling(&snapshot, "完全错误").unwrap().1);
        assert!(grade_spelling(&snapshot, "  ").is_err());
        assert!(grade_spelling(&snapshot, "a\nb").is_err());
        for (answer, expected_key) in [
            ("  It’s\u{3000}Well—Known  ", "it's well-known"),
            ("cafe\u{301}", "café"),
            ("ＣＥＮＴＥＲ", "center"),
        ] {
            let historical = AnswerSnapshot {
                custom_word: None,
                wordlist_fallback: false,
                choice: None,
                answers: vec![expected_key.into()],
                keys: vec![expected_key.into()],
                normalization_version: 1,
            };
            assert_eq!(
                grade_spelling(&historical, answer).unwrap(),
                (expected_key.to_owned(), true)
            );
        }
        assert!(grade_spelling(&snapshot, &"a".repeat(200)).is_ok());
        assert!(grade_spelling(&snapshot, &"a".repeat(201)).is_err());
        assert!(grade_spelling(&snapshot, &format!("{}a", " ".repeat(399))).is_ok());
        assert!(grade_spelling(&snapshot, &format!("{}a", " ".repeat(400))).is_err());
        assert!(grade_spelling(&snapshot, &"ﬃ".repeat(67)).is_err());
        let unsupported = AnswerSnapshot {
            normalization_version: 2,
            ..snapshot
        };
        assert_eq!(
            grade_spelling(&unsupported, "don't").unwrap_err().code(),
            ErrorCode::LearningRunUnavailable
        );
        assert!(leaks("猫 cat", "cat"));
        assert!(leaks("cat×示例", "cat"));
        assert!(leaks(&format!("{} ＣＡＴ", "中".repeat(201)), "cat"));
        assert!(leaks("换行\nＤＯＮ’Ｔ", "don't"));
        assert!(!leaks("类别 category", "cat"));
    }
}
