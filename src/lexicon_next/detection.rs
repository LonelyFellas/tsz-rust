use super::{language, model::NextEntryKind};
use crate::{
    error::{AppError, ErrorCode},
    lexicon::normalization::{NormalizedHeadword, normalize_headword},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::BTreeSet;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDetectionInput {
    pub target_language: String,
    pub surface: String,
}

#[derive(Serialize, ToSchema)]
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

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDictionaryForm {
    pub text: String,
    pub tags: Vec<String>,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDictionaryPhonetic {
    pub ipa: String,
    pub tags: Vec<String>,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDetection {
    /// Display input, never replaced with a dictionary record's term.
    pub surface: String,
    pub kind: NextEntryKind,
    /// Independent source records; no primary record or automatic draft prefill.
    pub matches: Vec<NextDictionaryMatch>,
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

pub async fn detect(pool: &PgPool, input: NextDetectionInput) -> Result<NextDetection, AppError> {
    language::require_language(&input.target_language)?;
    let surface = NormalizedHeadword::parse(&input.surface).map_err(|_| language::invalid())?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let available: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dictionary.datasets d JOIN dictionary.content_imports i ON i.dataset_id=d.id WHERE d.status='active')")
        .fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    if !available {
        return Err(AppError::request_error(
            ErrorCode::ServiceUnavailable,
            "内置词典暂时不可用，请稍后重试。",
        ));
    }
    // Targets retain source spelling. PostgreSQL 18 Unicode folding finds a superset
    // of reverse links; the frozen Rust normalizer below decides actual equality.
    let regions = sqlx::query_as::<_, Region>(r#"
        SELECT normalized_term,targets FROM dictionary.active_region_surfaces
        WHERE normalized_term=$1 OR EXISTS (
            SELECT 1 FROM unnest(targets) target WHERE
            casefold(translate(btrim(regexp_replace(normalize(target,NFKC) COLLATE pg_unicode_fast,
                '[[:space:]]+',' ','g')), '‘’ʼ‐‑‒–—−',repeat(chr(39),3)||repeat('-',6)) COLLATE pg_unicode_fast)
                = casefold($1 COLLATE pg_unicode_fast)
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
    tx.commit().await.map_err(AppError::internal)?;
    Ok(NextDetection {
        kind: if surface.display.contains(' ') {
            NextEntryKind::Phrase
        } else {
            NextEntryKind::Word
        },
        surface: surface.display,
        matches: records.into_iter().map(Record::into_match).collect(),
    })
}
