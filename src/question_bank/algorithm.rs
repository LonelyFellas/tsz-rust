//! Pure selection rules over the published candidate index; never writes a question bank.
use super::{dto::*, source};
use crate::{error::AppError, learning_tasks::repository::Tx, platform::text::key_v1};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

const PAGE_SIZE: usize = 64;
#[derive(Default)]
struct Features {
    spelling_key: String,
    spelling: Vec<char>,
    definition_chars: HashSet<char>,
}
#[derive(sqlx::FromRow)]
struct IndexedCandidate {
    entry_id: Uuid,
    publication_id: Uuid,
    pos_id: Uuid,
    sense_id: Uuid,
    definition_id: Uuid,
    form_id: Uuid,
    spelling: String,
    accepted_keys: Vec<String>,
    definition_keys: Vec<String>,
    #[sqlx(skip)]
    features: Features,
}
#[derive(Default)]
struct PageCache {
    rows: Vec<IndexedCandidate>,
    after: Option<[Uuid; 5]>,
    exhausted: bool,
}
pub struct Pool {
    pages: HashMap<(String, Option<String>), PageCache>,
    current: HashMap<Uuid, Vec<QuestionCandidate>>,
    synonyms: HashMap<Uuid, (HashSet<Uuid>, HashSet<String>)>,
    level: String,
    profile: String,
}
impl Pool {
    pub async fn load(
        tx: &mut Tx<'_>,
        level: &str,
        profile: &str,
        parts: &[String],
    ) -> Result<Self, AppError> {
        source::validate_context(level, profile)?;
        let mut pool = Self {
            pages: HashMap::new(),
            current: HashMap::new(),
            synonyms: HashMap::new(),
            level: level.into(),
            profile: profile.into(),
        };
        for part in parts.iter().collect::<HashSet<_>>() {
            pool.load_next(tx, &(part.clone(), None)).await?;
        }
        Ok(pool)
    }
    async fn load_next(
        &mut self,
        tx: &mut Tx<'_>,
        key: &(String, Option<String>),
    ) -> Result<(), AppError> {
        let after = self.pages.get(key).and_then(|p| p.after);
        let mut query = sqlx::QueryBuilder::new(
            "SELECT c.entry_id,c.publication_id,c.pos_id,c.sense_id,c.definition_id,c.form_id,c.spelling,c.accepted_keys,c.definition_keys FROM lexicon.question_candidate_index c JOIN lexicon.entries e ON e.id=c.entry_id AND e.current_publication_id=c.publication_id WHERE e.archived_at IS NULL AND e.teaching_eligible AND c.answer_profile=",
        );
        query
            .push_bind(&self.profile)
            .push(" AND c.level<=")
            .push_bind(&self.level)
            .push(" AND c.part_of_speech=")
            .push_bind(&key.0);
        if let Some(cursor) = after {
            query.push(" AND (c.entry_id,c.pos_id,c.sense_id,c.definition_id,c.form_id)>(");
            let mut values = query.separated(",");
            for value in cursor {
                values.push_bind(value);
            }
            query.push(")");
        }
        if let Some(pattern) = &key.1 {
            query.push(" AND c.spelling ILIKE ").push_bind(pattern);
        }
        query
            .push(" ORDER BY c.entry_id,c.pos_id,c.sense_id,c.definition_id,c.form_id LIMIT ")
            .push_bind(PAGE_SIZE as i64);
        let mut rows: Vec<IndexedCandidate> = query
            .build_query_as()
            .fetch_all(&mut **tx)
            .await
            .map_err(AppError::internal)?;
        let exhausted = rows.len() < PAGE_SIZE;
        for row in &mut rows {
            row.features = Features {
                spelling_key: key_v1(&row.spelling),
                spelling: row.spelling.chars().collect(),
                definition_chars: row
                    .definition_keys
                    .iter()
                    .flat_map(|d| definition_chars(d))
                    .collect(),
            };
        }
        let page = self.pages.entry(key.clone()).or_default();
        if let Some(last) = rows.last() {
            page.after = Some([
                last.entry_id,
                last.pos_id,
                last.sense_id,
                last.definition_id,
                last.form_id,
            ]);
        }
        page.exhausted = exhausted;
        page.rows.extend(rows);
        Ok(())
    }
    pub async fn content(
        &mut self,
        tx: &mut Tx<'_>,
        target: &QuestionCandidate,
        level: &str,
    ) -> Result<Option<BankQuestionContent>, AppError> {
        if let std::collections::hash_map::Entry::Vacant(e) =
            self.synonyms.entry(target.source.entry_id)
        {
            e.insert(source::synonym_refs(tx, target.source.entry_id).await?);
        }
        let target_key = key_v1(&target.spelling);
        let spelling: Vec<_> = target.spelling.chars().collect();
        let target_definition = key_v1(&target.definition);
        let target_chars = definition_chars(&target_definition);
        let prefix: String = target_key
            .chars()
            .take(2)
            .flat_map(|c| {
                if matches!(c, '%' | '_' | '\\') {
                    vec!['\\', c]
                } else {
                    vec![c]
                }
            })
            .collect();
        let mut spelling_keys: HashSet<_> = target
            .accepted_spellings
            .iter()
            .chain(std::iter::once(&target.spelling))
            .map(|s| key_v1(s))
            .collect();
        let mut entries = HashSet::from([target.source.entry_id]);
        let mut choices = Vec::new();
        // Bounded spelling-prefix recall first, then the shared POS pool. Both paginate to exhaustion.
        for pattern in [Some(format!("{prefix}%")), None] {
            let key = (target.part_of_speech.clone(), pattern);
            let mut start = 0;
            loop {
                if !self.pages.contains_key(&key)
                    || (start == self.pages[&key].rows.len() && !self.pages[&key].exhausted)
                {
                    self.load_next(tx, &key).await?;
                }
                let page = &self.pages[&key];
                let end = (start + PAGE_SIZE).min(page.rows.len());
                if start == end {
                    break;
                }
                let mut ranked: Vec<_> = page.rows[start..end]
                    .iter()
                    .filter(|r| {
                        !r.spelling.trim().is_empty()
                            && !entries.contains(&r.entry_id)
                            && r.features.definition_chars.is_disjoint(&target_chars)
                            && !r.definition_keys.iter().any(|d| {
                                d == &target_definition
                                    || d.contains(&target_definition)
                                    || target_definition.contains(d)
                            })
                    })
                    .collect();
                ranked.sort_by_cached_key(|r| {
                    (
                        std::cmp::Reverse(
                            spelling
                                .iter()
                                .zip(&r.features.spelling)
                                .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
                                .count(),
                        ),
                        spelling.len().abs_diff(r.features.spelling.len()),
                        r.entry_id,
                        r.sense_id,
                        r.definition_id,
                        r.form_id,
                    )
                });
                for r in ranked {
                    if entries.contains(&r.entry_id)
                        || self.synonyms[&target.source.entry_id]
                            .0
                            .contains(&r.entry_id)
                        || self.synonyms[&target.source.entry_id]
                            .1
                            .contains(&r.features.spelling_key)
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
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        self.synonyms.entry(r.entry_id)
                    {
                        e.insert(source::synonym_refs(tx, r.entry_id).await?);
                    }
                    if self.synonyms[&r.entry_id]
                        .0
                        .contains(&target.source.entry_id)
                        || self.synonyms[&r.entry_id].1.contains(&target_key)
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
                if choices.len() == 3 {
                    break;
                }
                start = end;
                if start == page.rows.len() && page.exhausted {
                    break;
                }
            }
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
