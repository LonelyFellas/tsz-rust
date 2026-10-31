//! Pure selection rules over the published candidate index; never writes a question bank.
use super::{
    dto::*,
    semantics::{self, Evidence},
    source,
};
use crate::{error::AppError, learning_tasks::repository::Tx, platform::text::key_v1};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
type Quality = (u16, Reverse<usize>, Reverse<usize>);
use uuid::Uuid;

const PAGE_SIZE: usize = 64;
#[derive(Default)]
struct Features {
    spelling_key: String,
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
    level: String,
    #[sqlx(skip)]
    features: Features,
}
#[derive(Default)]
struct PageCache {
    rows: Vec<IndexedCandidate>,
    after: Option<[Uuid; 5]>,
    exhausted: bool,
}
#[derive(Clone, Hash, Eq, PartialEq)]
struct Recall {
    pos: String,
    domains: Vec<String>,
    related: Vec<String>,
    definition: String,
}
pub struct Pool {
    pub supplemental_entries: Vec<Uuid>,
    pub failure_reason: Option<&'static str>,
    pages: HashMap<Recall, PageCache>,
    evidence: HashMap<String, Arc<Evidence>>,
    current: HashMap<Uuid, Vec<QuestionCandidate>>,
    synonyms: HashMap<Uuid, (HashSet<Uuid>, HashSet<String>)>,
    level: String,
    profile: String,
}
impl Pool {
    pub async fn load(tx: &mut Tx<'_>, level: &str, profile: &str) -> Result<Self, AppError> {
        source::validate_context(level, profile)?;
        let _: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM dictionary.datasets WHERE status='active' ORDER BY id FOR SHARE",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
        Ok(Self {
            supplemental_entries: vec![],
            failure_reason: None,
            pages: HashMap::new(),
            evidence: HashMap::new(),
            current: HashMap::new(),
            synonyms: HashMap::new(),
            level: level.into(),
            profile: profile.into(),
        })
    }
    async fn load_next(&mut self, tx: &mut Tx<'_>, key: &Recall) -> Result<(), AppError> {
        let after = self.pages.get(key).and_then(|p| p.after);
        let mut query = sqlx::QueryBuilder::new(
            "SELECT c.entry_id,c.publication_id,c.pos_id,c.sense_id,c.definition_id,c.form_id,c.spelling,c.accepted_keys,c.definition_keys,c.level FROM lexicon.question_candidate_index c JOIN lexicon.entries e ON e.id=c.entry_id AND e.current_publication_id=c.publication_id WHERE e.archived_at IS NULL AND e.teaching_eligible AND c.answer_profile=",
        );
        query
            .push_bind(&self.profile)
            .push(" AND c.level<=")
            .push_bind(&self.level)
            .push(" AND c.part_of_speech=")
            .push_bind(&key.pos);
        query
            .push(" AND NOT (")
            .push_bind(&key.definition)
            .push("=ANY(c.definition_keys))");
        if let Some(cursor) = after {
            query.push(" AND (c.entry_id,c.pos_id,c.sense_id,c.definition_id,c.form_id)>(");
            let mut values = query.separated(",");
            for value in cursor {
                values.push_bind(value);
            }
            query.push(")");
        }
        if !key.related.is_empty() {
            query
                .push(" AND lexicon.question_candidate_key_v1(c.spelling)=ANY(")
                .push_bind(&key.related)
                .push("::text[])");
        } else if !key.domains.is_empty() {
            query.push(" AND EXISTS(SELECT 1 FROM dictionary.choice_semantics sem JOIN dictionary.datasets d ON d.id=sem.dataset_id WHERE d.status='active' AND sem.normalized_term=lexicon.question_candidate_key_v1(c.spelling) AND sem.pos=c.part_of_speech AND sem.domains && ").push_bind(&key.domains).push("::text[])");
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
        supplement: bool,
    ) -> Result<Option<BankQuestionContent>, AppError> {
        self.supplemental_entries.clear();
        self.failure_reason = Some("insufficient_distractors");
        if let std::collections::hash_map::Entry::Vacant(e) =
            self.synonyms.entry(target.source.entry_id)
        {
            e.insert(source::synonym_refs(tx, target.source.entry_id).await?);
        }
        let target_key = key_v1(&target.spelling);
        let spelling: Vec<_> = target.spelling.chars().collect();
        let evidence_key = target
            .accepted_spellings
            .iter()
            .map(|s| key_v1(s))
            .collect::<Vec<_>>()
            .join("\n");
        if !self.evidence.contains_key(&evidence_key) {
            self.evidence.insert(
                evidence_key.clone(),
                Arc::new(Evidence::load(tx, &target.accepted_spellings).await?),
            );
        }
        let target_evidence = self.evidence[&evidence_key].clone();
        let Some(context) = target_evidence.context(target) else {
            self.failure_reason = Some(if target_evidence.complete {
                "target_unaligned"
            } else {
                "evidence_incomplete"
            });
            return Ok(None);
        };
        let mut domains: Vec<_> = context.domains.iter().cloned().collect();
        domains.sort();
        let keys: Vec<_> = target
            .accepted_spellings
            .iter()
            .chain(std::iter::once(&target.spelling))
            .map(|s| key_v1(s))
            .collect();
        let incoming:Vec<String>=sqlx::query_scalar("SELECT DISTINCT sem.normalized_term FROM dictionary.choice_semantics sem JOIN dictionary.datasets d ON d.id=sem.dataset_id WHERE d.status='active' AND sem.pos=$1 AND sem.antonyms && $2::text[] ORDER BY sem.normalized_term")
            .bind(&target.part_of_speech).bind(&keys).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
        let mut related: Vec<_> = context.related.iter().cloned().chain(incoming).collect();
        related.sort();
        related.dedup();
        let mut recalls = Vec::new();
        if !related.is_empty() {
            recalls.push(Recall {
                pos: target.part_of_speech.clone(),
                domains: vec![],
                related,
                definition: key_v1(&target.definition),
            });
        }
        if !domains.is_empty() {
            recalls.push(Recall {
                pos: target.part_of_speech.clone(),
                domains,
                related: vec![],
                definition: key_v1(&target.definition),
            });
        }
        if supplement && supplement_allowed(level, &target.part_of_speech, &self.profile) {
            recalls.push(Recall {
                pos: target.part_of_speech.clone(),
                domains: vec![],
                related: vec![],
                definition: key_v1(&target.definition),
            });
        }
        let spelling_keys: HashSet<_> = target
            .accepted_spellings
            .iter()
            .chain(std::iter::once(&target.spelling))
            .map(|s| key_v1(s))
            .collect();
        let entries = HashSet::from([target.source.entry_id]);
        let mut choices: HashMap<Uuid, (Quality, QuestionCandidate)> = HashMap::new();
        // Explicit relations use indexed word lookup; domain pages are shared across targets.
        for key in recalls {
            let supplementary = key.domains.is_empty() && key.related.is_empty();
            if supplementary && best_choices(&choices, target).len() == 3 {
                break;
            }
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
                let ranked: Vec<_> = page.rows[start..end]
                    .iter()
                    .filter(|r| {
                        !r.spelling.trim().is_empty()
                            && !entries.contains(&r.entry_id)
                            && !r
                                .definition_keys
                                .iter()
                                .any(|d| semantics::meanings_overlap(&target.definition, d))
                    })
                    .collect();
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
                    let candidate_key = c
                        .accepted_spellings
                        .iter()
                        .map(|s| key_v1(s))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !self.evidence.contains_key(&candidate_key) {
                        self.evidence.insert(
                            candidate_key.clone(),
                            Arc::new(Evidence::load(tx, &c.accepted_spellings).await?),
                        );
                    }
                    let evidence = &self.evidence[&candidate_key];
                    if target_evidence.conflicts(evidence, target, c) {
                        continue;
                    }
                    // Alignment/completeness remains mandatory even when semantic relatedness is optional.
                    if evidence.context(c).is_none() {
                        continue;
                    }
                    let quality = match evidence.quality(&context, target, c) {
                        Some(quality) => quality,
                        None if supplementary => 0,
                        None => continue,
                    };
                    let score = (
                        quality,
                        Reverse(level_distance(&target.level, &r.level)),
                        Reverse(
                            spelling
                                .len()
                                .abs_diff(r.features.spelling_key.chars().count()),
                        ),
                    );
                    match choices.entry(r.entry_id) {
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert((score, c.clone()));
                        }
                        std::collections::hash_map::Entry::Occupied(mut e) => {
                            if score > e.get().0 {
                                e.insert((score, c.clone()));
                            }
                        }
                    }
                }
                // A bounded window of distinct entries; continue if spelling aliases still prevent a triple.
                if choices.len() >= 24 && best_choices(&choices, target).len() == 3 {
                    break;
                }
                start = end;
                if start == page.rows.len() && page.exhausted {
                    break;
                }
            }
        }
        let supplemental: HashSet<_> = choices
            .iter()
            .filter(|(_, (score, _))| score.0 == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut choices = best_choices(&choices, target);
        if choices.len() != 3 {
            return Ok(None);
        }
        self.supplemental_entries = choices
            .iter()
            .filter(|c| supplemental.contains(&c.source.entry_id))
            .map(|c| c.source.entry_id)
            .collect();
        self.failure_reason = None;
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
fn best_choices(
    choices: &HashMap<Uuid, (Quality, QuestionCandidate)>,
    target: &QuestionCandidate,
) -> Vec<QuestionCandidate> {
    let mut ranked: Vec<_> = choices.values().collect();
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.source.entry_id.cmp(&b.1.source.entry_id))
    });
    let mut used: HashSet<_> = target
        .accepted_spellings
        .iter()
        .chain(std::iter::once(&target.spelling))
        .map(|s| key_v1(s))
        .collect();
    let mut selected = Vec::new();
    for (_, c) in ranked {
        let keys: HashSet<_> = c
            .accepted_spellings
            .iter()
            .chain(std::iter::once(&c.spelling))
            .map(|s| key_v1(s))
            .collect();
        if !keys.is_disjoint(&used) {
            continue;
        }
        used.extend(keys);
        selected.push(c.clone());
        if selected.len() == 3 {
            break;
        }
    }
    selected
}
fn level_distance(a: &str, b: &str) -> usize {
    let levels = ["A1", "A2", "B1", "B2", "C1", "C2"];
    levels
        .iter()
        .position(|s| *s == a)
        .unwrap_or(0)
        .abs_diff(levels.iter().position(|s| *s == b).unwrap_or(0))
}
pub fn definitions_overlap(a: &str, b: &str) -> bool {
    semantics::meanings_overlap(a, b)
}

/// Versioned retirement boundary. Close a reviewed level/POS/profile cohort here;
/// dictionary size never toggles policy automatically. See tsz tiered-distractors-plan.md.
const SEMANTIC_ONLY_COHORTS: &[(&str, &str, &str)] = &[];
fn supplement_allowed(level: &str, pos: &str, profile: &str) -> bool {
    !SEMANTIC_ONLY_COHORTS.contains(&(level, pos, profile))
}
