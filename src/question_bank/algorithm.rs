//! Pure selection rules over the published candidate index; never writes a question bank.
use super::{dto::*, source};
use crate::{error::AppError, learning_tasks::repository::Tx, platform::text::key_v1};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct IndexedCandidate {
    entry_id: Uuid,
    publication_id: Uuid,
    pos_id: Uuid,
    sense_id: Uuid,
    definition_id: Uuid,
    form_id: Uuid,
    spelling: String,
    part_of_speech: String,
    accepted_keys: Vec<String>,
    definition_keys: Vec<String>,
}
pub struct Pool {
    rows: Vec<IndexedCandidate>,
    current: HashMap<Uuid, Vec<QuestionCandidate>>,
    profile: String,
    synonyms: HashMap<Uuid, (HashSet<Uuid>, HashSet<String>)>,
}
impl Pool {
    pub async fn load(
        tx: &mut Tx<'_>,
        level: &str,
        profile: &str,
        parts: &[String],
    ) -> Result<Self, AppError> {
        source::validate_context(level, profile)?;
        let rows = sqlx::query_as("SELECT c.entry_id,c.publication_id,c.pos_id,c.sense_id,c.definition_id,c.form_id,c.spelling,c.part_of_speech,c.accepted_keys,c.definition_keys FROM lexicon.question_candidate_index c JOIN lexicon.entries e ON e.id=c.entry_id AND e.current_publication_id=c.publication_id WHERE e.archived_at IS NULL AND e.teaching_eligible AND c.answer_profile=$1 AND c.level<=$2 AND c.part_of_speech=ANY($3) ORDER BY c.entry_id,c.pos_id,c.sense_id,c.definition_id,c.form_id")
            .bind(profile).bind(level).bind(parts).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
        Ok(Self {
            rows,
            current: HashMap::new(),
            profile: profile.into(),
            synonyms: HashMap::new(),
        })
    }
    pub async fn content(
        &mut self,
        tx: &mut Tx<'_>,
        target: &QuestionCandidate,
        level: &str,
    ) -> Result<Option<BankQuestionContent>, AppError> {
        if let std::collections::hash_map::Entry::Vacant(entry) =
            self.synonyms.entry(target.source.entry_id)
        {
            entry.insert(source::synonym_refs(tx, target.source.entry_id).await?);
        }
        let mut spelling_keys: HashSet<_> = target
            .accepted_spellings
            .iter()
            .chain(std::iter::once(&target.spelling))
            .map(|s| key_v1(s))
            .collect();
        let mut entries = HashSet::from([target.source.entry_id]);
        let mut ranked: Vec<_> = self
            .rows
            .iter()
            .filter(|r| {
                !r.spelling.trim().is_empty()
                    && r.part_of_speech == target.part_of_speech
                    && r.entry_id != target.source.entry_id
                    && !r
                        .definition_keys
                        .iter()
                        .any(|d| definitions_overlap(&target.definition, d))
            })
            .collect();
        // Prefer similar spelling/length; source identity deterministically breaks ties.
        ranked.sort_by_cached_key(|r| {
            (
                std::cmp::Reverse(
                    target
                        .spelling
                        .chars()
                        .zip(r.spelling.chars())
                        .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
                        .count(),
                ),
                target
                    .spelling
                    .chars()
                    .count()
                    .abs_diff(r.spelling.chars().count()),
                r.entry_id,
                r.sense_id,
                r.definition_id,
                r.form_id,
            )
        });
        let mut choices = Vec::new();
        for r in ranked {
            if self.synonyms[&target.source.entry_id]
                .0
                .contains(&r.entry_id)
                || self.synonyms[&target.source.entry_id]
                    .1
                    .contains(&key_v1(&r.spelling))
                || entries.contains(&r.entry_id)
                || r.accepted_keys.iter().any(|k| spelling_keys.contains(k))
            {
                continue;
            }
            if !self.current.contains_key(&r.entry_id) {
                self.current.insert(
                    r.entry_id,
                    source::candidates(tx, &[r.entry_id], "C2", &self.profile).await?,
                );
            }
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.synonyms.entry(r.entry_id)
            {
                entry.insert(source::synonym_refs(tx, r.entry_id).await?);
            }
            if self.synonyms[&r.entry_id]
                .0
                .contains(&target.source.entry_id)
                || self.synonyms[&r.entry_id]
                    .1
                    .contains(&key_v1(&target.spelling))
            {
                continue;
            }
            let Some(c) = self.current[&r.entry_id].iter().find(|c| {
                c.source.publication_id == r.publication_id
                    && c.source.pos_id == r.pos_id
                    && c.source.sense_id == r.sense_id
                    && c.source.definition_id == r.definition_id
                    && c.source.form_id == r.form_id
            }) else {
                continue;
            };
            if c.other_definitions
                .iter()
                .chain(std::iter::once(&c.definition))
                .any(|d| definitions_overlap(&target.definition, d))
            {
                continue;
            }
            entries.insert(r.entry_id);
            spelling_keys.extend(c.accepted_spellings.iter().map(|s| key_v1(s)));
            choices.push(c.clone());
            if choices.len() == 3 {
                break;
            }
        }
        if choices.len() != 3 {
            return Ok(None);
        }
        choices.insert(0, target.clone());
        let options: Vec<_> = choices
            .into_iter()
            .enumerate()
            .map(|(i, c)| BankQuestionOption {
                option_id: ["A", "B", "C", "D"][i].into(),
                explanation: format!("{}：{}", c.spelling, c.definition),
                candidate: c,
            })
            .collect();
        Ok(Some(BankQuestionContent {
            target: target.clone(),
            prompt: target.definition.clone(),
            question_level: level.into(),
            answer_profile: self.profile.clone(),
            explanation: options
                .iter()
                .map(|o| o.explanation.clone())
                .collect::<Vec<_>>()
                .join("\n"),
            options,
            correct_option_id: "A".into(),
            concerns: vec![],
        }))
    }
}
fn definition_chars(value: &str) -> HashSet<char> {
    value
        .chars()
        .filter(|c| {
            ('\u{3400}'..='\u{9fff}').contains(c) && !"的地得一某种个人和与或及等".contains(*c)
        })
        .collect()
}
pub fn definitions_overlap(a: &str, b: &str) -> bool {
    let a_key = key_v1(a);
    let b_key = key_v1(b);
    if a_key == b_key || a_key.contains(&b_key) || b_key.contains(&a_key) {
        return true;
    }
    // Reject even partial known meaning overlap (e.g. 拿/带 in take and bring).
    !definition_chars(a).is_disjoint(&definition_chars(b))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn excludes_partial_answers_without_rejecting_every_adjective() {
        assert!(definitions_overlap("拿；取；带走", "带来；拿来"));
        assert!(definitions_overlap("结束；完成", "完成"));
        assert!(definitions_overlap("苦的,苦涩的", "苦涩的"));
        assert!(!definitions_overlap("苦的,苦涩的", "忙碌的"));
        assert!(!definitions_overlap("苹果", "香蕉"));
    }
}
