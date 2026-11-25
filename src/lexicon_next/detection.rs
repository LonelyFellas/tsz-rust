use super::{language, model::NextEntryKind};
use crate::{
    dictionary::pos,
    error::AppError,
    lexicon::normalization::{NormalizedHeadword, normalize_headword},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDetectionInput {
    pub target_language: String,
    pub surface: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDictionaryMatch {
    pub id: String,
    pub term: String,
    pub part_of_speech: String,
    pub glosses: Vec<String>,
    pub forms: Vec<NextDictionaryForm>,
    pub phonetics: Vec<NextDictionaryPhonetic>,
    pub source_name: String,
    pub source_version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDictionaryForm {
    pub text: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDictionaryPhonetic {
    pub ipa: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDetection {
    /// Display input, never replaced with a dictionary record's term.
    pub surface: String,
    pub kind: NextEntryKind,
    /// Independent source records; no primary record or automatic draft prefill.
    pub matches: Vec<NextDictionaryMatch>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDetectionResult {
    pub surface: String,
    pub kind: NextEntryKind,
    pub matches: Vec<NextDictionaryMatch>,
    /// Returned for clients opting in to catalog resolution. An empty list means no match resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub part_of_speech_mappings: Option<Vec<NextDictionaryPosMapping>>,
}

#[derive(Debug, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDictionaryPosMapping {
    pub source_code: String,
    pub part_of_speech: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sub_pos: Option<String>,
    /// A proper name alone does not imply the parent noun's inflection template.
    pub base_form_only: bool,
}

type PosCatalog = BTreeMap<String, Vec<String>>;

fn resolve_pos(
    source_code: &str,
    kind: &NextEntryKind,
    catalog: &PosCatalog,
) -> Option<NextDictionaryPosMapping> {
    let canonical = pos::canonical(source_code);
    let (code, sub_pos, base_form_only): (_, &[_], _) = match (kind, canonical) {
        _ if catalog.contains_key(source_code) => (source_code, &[], false),
        (NextEntryKind::Word, Some("proper_name")) => ("noun", &["NOUN-PROPER", "N-PROPER"], true),
        (NextEntryKind::Word, Some("article")) => ("determiner", &["ART"], false),
        (NextEntryKind::Word, Some("numeral")) if !catalog.contains_key("numeral") => {
            ("numural", &[], false)
        }
        (NextEntryKind::Word, Some("prepositional_phrase")) => return None,
        (NextEntryKind::Word, Some(code)) => (code, &[], false),
        (NextEntryKind::Phrase, Some("noun")) => ("phrase_noun", &[], false),
        (NextEntryKind::Phrase, Some("proper_name")) => ("phrase_noun", &[], true),
        (NextEntryKind::Phrase, Some("verb")) => ("phrase_verbal_constituent", &[], false),
        (NextEntryKind::Phrase, Some("adjective")) => ("phrase_adjectival_constituent", &[], false),
        (NextEntryKind::Phrase, Some("adverb")) => ("phrase_adverbial_constituent", &[], false),
        (NextEntryKind::Phrase, Some("pronoun")) => ("phrase_pronominal_constituent", &[], false),
        (NextEntryKind::Phrase, Some("preposition" | "prepositional_phrase")) => {
            ("phrase_prepositional_constituent", &[], false)
        }
        (NextEntryKind::Phrase, Some("determiner")) => {
            ("phrase_determinative_constituent", &[], false)
        }
        (NextEntryKind::Phrase, Some("conjunction")) => {
            ("phrase_conjunctive_constituent", &[], false)
        }
        (NextEntryKind::Phrase, Some("interjection")) => {
            ("phrase_interjectional_constituen", &[], false)
        }
        (NextEntryKind::Phrase, Some("numeral")) => ("phrase_numeral_constituent", &[], false),
        _ => (source_code, &[], false),
    };
    let sub_parts = catalog.get(code)?;
    Some(NextDictionaryPosMapping {
        source_code: source_code.to_owned(),
        part_of_speech: code.to_owned(),
        sub_pos: sub_pos
            .iter()
            .copied()
            .find(|sub| sub_parts.iter().any(|item| item == sub))
            .map(str::to_owned),
        base_form_only,
    })
}

#[derive(sqlx::FromRow)]
struct Record {
    id: String,
    term: String,
    pos: String,
    senses: Value,
    forms: Value,
    sounds: Value,
    source_name: String,
    source_version: String,
}

#[derive(sqlx::FromRow)]
struct Region {
    normalized_term: String,
    targets: Vec<String>,
}
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
impl Record {
    fn into_match(self) -> NextDictionaryMatch {
        NextDictionaryMatch {
            id: self.id,
            term: self.term,
            part_of_speech: self.pos,
            glosses: self
                .senses
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|sense| strings(&sense["glosses"]))
                .collect(),
            forms: self
                .forms
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|form| {
                    Some(NextDictionaryForm {
                        text: form["form"].as_str()?.to_owned(),
                        tags: strings(&form["tags"]),
                    })
                })
                .collect(),
            phonetics: self
                .sounds
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|sound| {
                    Some(NextDictionaryPhonetic {
                        ipa: sound["ipa"].as_str()?.to_owned(),
                        tags: strings(&sound["tags"]),
                    })
                })
                .collect(),
            source_name: self.source_name,
            source_version: self.source_version,
        }
    }
}

pub async fn detect(
    pool: &PgPool,
    input: NextDetectionInput,
    include_pos_mappings: bool,
) -> Result<NextDetectionResult, AppError> {
    language::require_language(&input.target_language)?;
    let surface = NormalizedHeadword::parse(&input.surface).map_err(|_| language::invalid())?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    // Targets retain source spelling. ICU Unicode lowercase works on PostgreSQL
    // 16 and 18; the frozen Rust normalizer below decides actual equality.
    let regions = sqlx::query_as::<_, Region>(r#"
        SELECT normalized_term,targets FROM dictionary.active_region_surfaces
        WHERE normalized_term=$1 OR EXISTS (
            SELECT 1 FROM unnest(targets) target WHERE
            lower(translate(btrim(regexp_replace(normalize(target,NFKC) COLLATE "und-x-icu",
                '[[:space:]]+',' ','g')), '‘’ʼ‐‑‒–—−',repeat(chr(39),3)||repeat('-',6)) COLLATE "und-x-icu")
                = lower($1 COLLATE "und-x-icu")
        )
    "#).bind(&surface.key).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let mut candidates = BTreeSet::from([surface.key.clone()]);
    for region in regions {
        let targets = region
            .targets
            .iter()
            .filter_map(|target| normalize_headword(target).ok().map(|value| value.key))
            .collect::<Vec<_>>();
        if region.normalized_term == surface.key {
            candidates.extend(targets);
        } else if targets.contains(&surface.key) {
            candidates.insert(region.normalized_term);
        }
    }
    // Match every directly linked spelling without selecting a headword. Only
    // lightweight regional metadata is folded; raw contents use the term index.
    let records = sqlx::query_as::<_, Record>(r#"
        SELECT c.dataset_id::text || ':' || c.source_key AS id,
               coalesce(t.term,c.normalized_term) AS term, c.pos,c.senses,c.forms,c.sounds,
               d.source_name,i.source_version
        FROM dictionary.entry_contents c
        JOIN dictionary.datasets d ON d.id=c.dataset_id AND d.status='active'
        JOIN dictionary.content_imports i ON i.dataset_id=c.dataset_id
        LEFT JOIN dictionary.terms t ON t.dataset_id=c.dataset_id AND t.normalized_term=c.normalized_term
        WHERE c.normalized_term = ANY($1)
        ORDER BY c.normalized_term,c.pos,c.source_key
    "#).bind(candidates.into_iter().collect::<Vec<_>>()).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let (kind, catalog_kind) = if surface.display.contains(' ') {
        (NextEntryKind::Phrase, "phrase")
    } else {
        (NextEntryKind::Word, "word")
    };
    let part_of_speech_mappings = if include_pos_mappings {
        let mappings = if records.is_empty() {
            Vec::new()
        } else {
            let catalog: PosCatalog = sqlx::query_as::<_, (String, Vec<String>)>(
                r#"
                SELECT p.code, ARRAY(
                    SELECT s.code FROM catalog.sub_parts_of_speech s WHERE s.part_of_speech_id = p.id
                )
                FROM catalog.parts_of_speech p
                WHERE p.kind = $1
                "#,
            )
            .bind(catalog_kind)
            .fetch_all(&mut *tx)
            .await
            .map_err(AppError::internal)?
            .into_iter()
            .collect();
            let mut seen = BTreeSet::new();
            records
                .iter()
                .map(|record| record.pos.as_str())
                .filter(|source_code| seen.insert(*source_code))
                .filter_map(|source_code| resolve_pos(source_code, &kind, &catalog))
                .collect()
        };
        Some(mappings)
    } else {
        None
    };
    tx.commit().await.map_err(AppError::internal)?;
    Ok(NextDetectionResult {
        kind,
        surface: surface.display,
        matches: records.into_iter().map(Record::into_match).collect(),
        part_of_speech_mappings,
    })
}
