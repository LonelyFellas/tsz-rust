//! Evidence extracted from the locally imported dictionary, never model inference.
use super::dto::QuestionCandidate;
use crate::{error::AppError, learning_tasks::repository::Tx, platform::text::key_v1};
use serde::Deserialize;
use sqlx::types::Json;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Meaning {
    #[serde(default = "supported_default")]
    supported: bool,
    tags: Vec<String>,
    glosses: Vec<String>,
    equivalents: Vec<String>,
    parents: Vec<String>,
    children: Vec<String>,
    coordinates: Vec<String>,
    antonyms: Vec<String>,
    translations: Vec<String>,
}
#[derive(sqlx::FromRow)]
struct Record {
    pos: String,
    normalized_term: String,
    meanings: Option<Json<Vec<Meaning>>>,
}
#[derive(Default, Clone)]
pub struct Evidence {
    pub complete: bool,
    senses: Vec<(String, Meaning)>,
    aliases: HashSet<String>,
    relatives: HashSet<String>,
    translations: HashSet<String>,
    glosses: Vec<HashSet<String>>,
    gloss_index: HashMap<String, Vec<usize>>,
}
#[derive(Default, Clone)]
pub struct Context {
    pub domains: HashSet<String>,
    pub related: HashSet<String>,
}
fn supported_default() -> bool {
    true
}
fn words(values: &[String]) -> HashSet<String> {
    values
        .iter()
        .map(|s| key_v1(s))
        .filter(|s| !s.is_empty())
        .collect()
}
fn informative_domain(s: &str) -> bool {
    !matches!(
        s,
        "entity"
            | "thing"
            | "object"
            | "person"
            | "individual"
            | "being"
            | "organism"
            | "act"
            | "action"
            | "event"
            | "state"
            | "quality"
            | "abstraction"
            | "relation"
            | "process"
            | "sciences"
            | "science"
            | "lifestyle"
            | "hobbies"
            | "general"
            | "countable"
            | "uncountable"
    ) && s.len() > 1
}
/// Whole gloss alternatives, not individual shared Chinese characters. Negation is preserved.
pub fn atoms(value: &str) -> HashSet<String> {
    key_v1(value)
        .split(['；', ';', ',', '，', '、', '/', '／', '|', '\n'])
        .map(|s| {
            s.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .map(|s| s.trim_end_matches('的').to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}
pub fn meanings_overlap(a: &str, b: &str) -> bool {
    let a = atoms(a);
    let b = atoms(b);
    a.iter().any(|x| {
        b.iter().any(|y| {
            x == y
                || (x.chars().count() >= 2
                    && y.chars().count() >= 2
                    && (x.contains(y) || y.contains(x)))
        })
    })
}
fn gloss_tokens(value: &str) -> HashSet<String> {
    key_v1(value)
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| {
            w.len() > 1
                && !matches!(
                    *w,
                    "the"
                        | "an"
                        | "to"
                        | "of"
                        | "or"
                        | "and"
                        | "in"
                        | "on"
                        | "at"
                        | "for"
                        | "with"
                        | "by"
                        | "from"
                        | "that"
                        | "which"
                        | "who"
                        | "as"
                        | "is"
                        | "be"
                        | "a"
                        | "any"
                        | "something"
                        | "someone"
                )
        })
        .map(|s| s.to_owned())
        .collect()
}
fn equivalence_terms(m: &Meaning) -> HashSet<String> {
    let mut aliases = words(&m.equivalents);
    for gloss in &m.glosses {
        let normalized = key_v1(gloss);
        for prefix in [
            "synonym of ",
            "alternative form of ",
            "alternative spelling of ",
            "variant of ",
            "same as ",
        ] {
            if let Some(rest) = normalized.strip_prefix(prefix) {
                let term = rest.split(['(', ';', '.']).next().unwrap_or("").trim();
                if !term.is_empty() {
                    aliases.insert(term.into());
                }
            }
        }
    }
    aliases
}
impl Evidence {
    fn from_senses(complete: bool, senses: Vec<(String, Meaning)>) -> Self {
        let aliases = senses
            .iter()
            .flat_map(|(_, m)| equivalence_terms(m))
            .collect();
        let relatives = senses
            .iter()
            .flat_map(|(_, m)| m.parents.iter().chain(&m.children))
            .map(|s| key_v1(s))
            .collect();
        let translations = senses
            .iter()
            .flat_map(|(_, m)| m.translations.iter().flat_map(|s| atoms(s)))
            .collect();
        let glosses: Vec<_> = senses
            .iter()
            .flat_map(|(_, m)| m.glosses.iter().map(|s| gloss_tokens(s)))
            .collect();
        let mut gloss_index: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, gloss) in glosses.iter().enumerate() {
            for word in gloss {
                gloss_index.entry(word.clone()).or_default().push(i);
            }
        }
        let complete = complete && senses.iter().all(|(_, m)| m.supported);
        Self {
            complete,
            senses,
            aliases,
            relatives,
            translations,
            glosses,
            gloss_index,
        }
    }
    pub async fn load(tx: &mut Tx<'_>, spellings: &[String]) -> Result<Self, AppError> {
        // A missing/oversized projection invalidates the whole spelling: never ignore an unknown sense.
        let keys: Vec<_> = spellings.iter().map(|s| key_v1(s)).collect();
        let records:Vec<Record>=sqlx::query_as("SELECT CASE c.pos WHEN 'adj' THEN 'adjective' WHEN 'adv' THEN 'adverb' WHEN 'prep' THEN 'preposition' WHEN 'pron' THEN 'pronoun' WHEN 'det' THEN 'determiner' WHEN 'conj' THEN 'conjunction' WHEN 'intj' THEN 'interjection' WHEN 'num' THEN 'numeral' ELSE c.pos END AS pos,c.normalized_term,s.meanings FROM dictionary.entry_contents c JOIN dictionary.datasets d ON d.id=c.dataset_id LEFT JOIN dictionary.choice_semantics s ON s.dataset_id=c.dataset_id AND s.source_key=c.source_key WHERE d.status='active' AND c.normalized_term=ANY($1) ORDER BY c.source_key FOR SHARE OF c")
            .bind(&keys).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
        let represented: HashSet<_> = records.iter().map(|r| r.normalized_term.clone()).collect();
        let complete = !keys.is_empty()
            && keys.iter().all(|k| represented.contains(k))
            && records.iter().all(|r| {
                r.meanings.as_ref().is_some_and(|m| {
                    !m.is_empty()
                        && m.iter().all(|s| {
                            s.supported && (!s.glosses.is_empty() || !s.equivalents.is_empty())
                        })
                })
            });
        let mut senses = Vec::new();
        for r in records {
            if let Some(m) = r.meanings {
                senses.extend(m.0.into_iter().map(|s| (r.pos.clone(), s)));
            }
        }
        Ok(Self::from_senses(complete, senses))
    }
    pub fn context(&self, target: &QuestionCandidate) -> Option<Context> {
        if !self.complete {
            return None;
        }
        let target_atoms = atoms(&target.definition);
        let mut matching: Vec<_> = self
            .senses
            .iter()
            .filter(|(pos, m)| {
                pos == &target.part_of_speech
                    && !m.tags.iter().any(|t| {
                        matches!(t.as_str(), "form-of" | "alt-of" | "obsolete" | "archaic")
                    })
                    && m.translations
                        .iter()
                        .any(|t| !atoms(t).is_disjoint(&target_atoms))
            })
            .map(|(_, m)| m)
            .collect();
        // Unambiguous single-sense entries can still be used when Chinese translations are absent.
        if matching.is_empty() {
            let all: Vec<_> = self
                .senses
                .iter()
                .filter(|(p, m)| {
                    p == &target.part_of_speech
                        && !m.glosses.is_empty()
                        && !m.tags.iter().any(|t| {
                            matches!(t.as_str(), "form-of" | "alt-of" | "obsolete" | "archaic")
                        })
                })
                .map(|(_, m)| m)
                .collect();
            if all.len() != 1 || !all[0].translations.is_empty() {
                return None;
            }
            matching = all;
        }
        let mut context = Context::default();
        for m in matching {
            context.domains.extend(
                m.parents
                    .iter()
                    .map(|s| key_v1(s))
                    .filter(|s| informative_domain(s)),
            );
            context.related.extend(words(&m.antonyms));
            context.related.extend(words(&m.coordinates));
        }
        Some(context)
    }
    pub fn conflicts(
        &self,
        other: &Self,
        target: &QuestionCandidate,
        candidate: &QuestionCandidate,
    ) -> bool {
        if !self.complete || !other.complete {
            return true;
        }
        let a_keys: HashSet<_> = target
            .accepted_spellings
            .iter()
            .chain(std::iter::once(&target.spelling))
            .map(|s| key_v1(s))
            .collect();
        let b_keys: HashSet<_> = candidate
            .accepted_spellings
            .iter()
            .chain(std::iter::once(&candidate.spelling))
            .map(|s| key_v1(s))
            .collect();
        let a_aliases: HashSet<_> = self.aliases.union(&a_keys).cloned().collect();
        let b_aliases: HashSet<_> = other.aliases.union(&b_keys).cloned().collect();
        if !a_aliases.is_disjoint(&b_aliases)
            || !self.relatives.is_disjoint(&b_keys)
            || !other.relatives.is_disjoint(&a_keys)
        {
            return true;
        }
        if other
            .translations
            .iter()
            .any(|t| meanings_overlap(&target.definition, t))
        {
            return true;
        }
        if self
            .glosses
            .iter()
            .any(|g| g.len() == 1 && !g.is_disjoint(&b_aliases))
        {
            return true;
        }
        for gloss in &other.glosses {
            if gloss.len() == 1 && gloss.iter().any(|w| a_aliases.contains(w)) {
                return true;
            }
            let mut shared: HashMap<usize, usize> = HashMap::new();
            for word in gloss {
                if let Some(ids) = self.gloss_index.get(word) {
                    for id in ids {
                        *shared.entry(*id).or_default() += 1;
                    }
                }
            }
            if shared
                .into_iter()
                .any(|(i, n)| n >= 2 && n * 4 >= (self.glosses[i].len() + gloss.len() - n) * 3)
            {
                return true;
            }
        }
        false
    }
    pub fn quality(
        &self,
        context: &Context,
        target: &QuestionCandidate,
        candidate: &QuestionCandidate,
    ) -> Option<u16> {
        let c = self.context(candidate)?;
        let direct = context.related.contains(&key_v1(&candidate.spelling))
            || c.related.contains(&key_v1(&target.spelling));
        let common = context.domains.intersection(&c.domains).count();
        if !direct && common == 0 {
            return None;
        }
        // Shared hypernyms and explicit contrast come before level/spelling tie breakers.
        Some((if direct { 100 } else { 0 }) + (common.min(10) as u16) * 10)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whole_meanings_replace_shared_character_veto() {
        assert!(!meanings_overlap("牛奶；奶", "奶酪；干酪"));
        assert!(!meanings_overlap("医生", "学生"));
        assert!(meanings_overlap("结束；完成", "完成"));
        assert!(meanings_overlap("苦涩的", "苦涩"));
        assert!(!meanings_overlap("有声的", "无声的"));
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::super::dto::QuestionSource;
    use super::*;
    use serde_json::json;
    use uuid::Uuid;
    fn candidate(word: &str, definition: &str) -> QuestionCandidate {
        QuestionCandidate {
            candidate_ref: word.into(),
            content_fingerprint: word.into(),
            spelling: word.into(),
            part_of_speech: "noun".into(),
            level: "A1".into(),
            definition: definition.into(),
            other_definitions: vec![],
            accepted_spellings: vec![word.into()],
            answer_profile: "BrE".into(),
            form_type: "base".into(),
            teaching_eligible: true,
            source: QuestionSource {
                entry_id: Uuid::now_v7(),
                publication_id: Uuid::nil(),
                pos_id: Uuid::nil(),
                sense_id: Uuid::nil(),
                definition_id: Uuid::nil(),
                form_id: Uuid::nil(),
            },
        }
    }
    fn evidence(values: serde_json::Value) -> Evidence {
        Evidence::from_senses(
            true,
            serde_json::from_value::<Vec<Meaning>>(values)
                .unwrap()
                .into_iter()
                .map(|m| ("noun".into(), m))
                .collect(),
        )
    }
    #[test]
    fn semantic_siblings_qualify_while_spelling_neighbours_do_not() {
        let apple = candidate("apple", "苹果");
        let orange = candidate("orange", "橙子");
        let airport = candidate("airport", "机场");
        let a = evidence(
            json!([{"parents":["fruit"],"glosses":["red orchard fruit"],"translations":["苹果"]}]),
        );
        let b = evidence(
            json!([{"parents":["fruit"],"glosses":["juicy citrus fruit"],"translations":["橙子"]}]),
        );
        let c = evidence(
            json!([{"parents":["transport facility"],"glosses":["aircraft landing facility"],"translations":["机场"]}]),
        );
        let ctx = a.context(&apple).unwrap();
        assert!(b.quality(&ctx, &apple, &orange).is_some());
        assert!(!a.conflicts(&b, &apple, &orange));
        assert!(c.quality(&ctx, &apple, &airport).is_none());
    }
    #[test]
    fn synonyms_alt_forms_and_hidden_second_senses_are_rejected() {
        let a = candidate("finish", "结束；完成");
        let b = candidate("terminate", "终止");
        let target =
            evidence(json!([{"glosses":["bring activity to an end"],"equivalents":["end"]}]));
        for hidden in [
            json!({"equivalents":["end"]}),
            json!({"equivalents":["finish"]}),
            json!({"translations":["完成"]}),
            json!({"glosses":["To finish."]}),
        ] {
            let other = evidence(
                json!([{"parents":["activity"],"glosses":["perform ordinary work"]},hidden]),
            );
            assert!(target.conflicts(&other, &a, &b));
        }
    }
    #[test]
    fn ambiguous_unaligned_and_missing_dictionary_evidence_fails_closed() {
        let bank = candidate("bank", "银行");
        let e = evidence(
            json!([{"parents":["financial institution"],"glosses":["institution accepting deposits"]},{"parents":["landform"],"glosses":["land alongside a river"]}]),
        );
        assert!(e.context(&bank).is_none());
        assert!(Evidence::default().context(&bank).is_none());
        let mismatched = evidence(
            json!([{"parents":["financial institution"],"glosses":["institution accepting deposits"],"translations":["金融机构"]}]),
        );
        assert!(mismatched.context(&bank).is_none());
        let aligned = evidence(
            json!([{"parents":["financial institution"],"glosses":["institution accepting deposits"],"translations":["银行"]},{"parents":["landform"],"glosses":["land alongside a river"]}]),
        );
        assert_eq!(
            aligned.context(&bank).unwrap().domains,
            HashSet::from(["financial institution".into()])
        );
    }
    #[test]
    fn broad_parent_answers_and_equivalent_english_glosses_are_rejected() {
        let a = candidate("apple", "苹果");
        let b = candidate("fruit", "水果");
        let e = evidence(json!([{"parents":["fruit"],"glosses":["red orchard fruit"]}]));
        let f = evidence(json!([{"glosses":["edible plant product"]}]));
        assert!(e.conflicts(&f, &a, &b));
        let single = evidence(json!([{"glosses":["pear"]}]));
        assert!(single.conflicts(&f, &a, &candidate("pear", "梨")));
        let other = evidence(json!([{"glosses":["a red orchard fruit"]}]));
        assert!(e.conflicts(&other, &a, &candidate("untracked_synonym", "别名")));
    }
}
