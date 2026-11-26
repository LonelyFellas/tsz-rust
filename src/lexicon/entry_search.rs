//! Shared published entry autocomplete. Callers retain realm/permission checks.
use crate::{
    error::{AppError, ErrorCode},
    lexicon::{
        normalization::{NormalizedHeadword, normalize_headword},
        published,
    },
};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;
type Tx<'a> = Transaction<'a, Postgres>;
#[derive(Clone, sqlx::FromRow)]
pub(crate) struct EntrySuggestionSummary {
    pub is_next: bool,
    pub entry_id: Uuid,
    pub publication_id: Uuid,
    pub label: String,
    pub glosses: Vec<String>,
    pub sense_count: i64,
}
const LIMIT: usize = 3;
const EXACT: &str = include_str!("search_sql/suggest_exact.sql");
const PREFIX: &str = include_str!("search_sql/suggest_prefix.sql");
const FUZZY_WORD: &str = include_str!("search_sql/suggest_fuzzy_word.sql");
const FUZZY: &str = include_str!("search_sql/suggest_fuzzy.sql");
const SUMMARY: &str = include_str!("search_sql/suggest_summary.sql");

/// Batch exact matches only. Ambiguous spellings remain ambiguous even when
/// their display labels differ (aliases and regional forms share the index).
pub(crate) async fn exact_batch(
    tx: &mut Tx<'_>,
    words: &[String],
) -> Result<Vec<Vec<EntrySuggestionSummary>>, AppError> {
    let keys = words
        .iter()
        .map(|word| normalize_headword(word).map(|word| word.key))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AppError::bad_request(ErrorCode::InvalidQuery, "请输入 1–200 字的词条名称"))?;
    let matches: Vec<(i64, Uuid, Uuid)> =
        sqlx::query_as(include_str!("search_sql/import_exact.sql"))
            .bind(&keys)
            .fetch_all(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    let ids: Vec<_> = matches.iter().map(|(_, id, _)| *id).collect();
    published::lock_learning_entries(tx, &ids).await?;
    let summaries: Vec<EntrySuggestionSummary> = sqlx::query_as(SUMMARY)
        .bind(&ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    let summaries: std::collections::HashMap<_, _> = summaries
        .into_iter()
        .map(|item| (item.entry_id, item))
        .collect();
    // A publication swap while acquiring locks must not turn an ambiguous
    // spelling into an apparently unique match. Match that input again instead.
    let changed_rows: std::collections::HashSet<_> = matches
        .iter()
        .filter(|(_, id, publication)| {
            summaries
                .get(id)
                .is_none_or(|item| item.publication_id != *publication)
        })
        .map(|(row, _, _)| *row)
        .collect();
    let mut rows = vec![vec![]; words.len()];
    for (row, id, publication) in matches {
        if changed_rows.contains(&row) {
            continue;
        }
        if let Some(item) = summaries
            .get(&id)
            .filter(|item| item.publication_id == publication)
        {
            rows[row as usize - 1].push(item.clone());
        }
    }
    Ok(rows)
}
fn exclusions(hits: &[(Uuid, Uuid)], exclude: Option<Uuid>) -> Vec<Uuid> {
    hits.iter().map(|(id, _)| *id).chain(exclude).collect()
}
async fn append(
    tx: &mut Tx<'_>,
    sql: &'static str,
    query: &str,
    hits: &mut Vec<(Uuid, Uuid)>,
    exclude: Option<Uuid>,
) -> Result<(), AppError> {
    let found: Vec<(Uuid, Uuid)> = sqlx::query_as(sql)
        .bind(query)
        .bind(exclusions(hits, exclude))
        .bind((LIMIT - hits.len()) as i64)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    hits.extend(found);
    Ok(())
}
pub(crate) async fn search(
    tx: &mut Tx<'_>,
    q: &str,
    exclude: Option<Uuid>,
) -> Result<Vec<EntrySuggestionSummary>, AppError> {
    search_sources(tx, q, exclude, false).await
}

/// Reviewed publications are selectable only in the student wordlist picker.
pub(crate) async fn search_wordlist(
    tx: &mut Tx<'_>,
    q: &str,
) -> Result<Vec<EntrySuggestionSummary>, AppError> {
    search_sources(tx, q, None, true).await
}

async fn search_sources(
    tx: &mut Tx<'_>,
    q: &str,
    exclude: Option<Uuid>,
    wordlist: bool,
) -> Result<Vec<EntrySuggestionSummary>, AppError> {
    let (exact, prefix, fuzzy_word, fuzzy_phrase) = if wordlist {
        (
            include_str!("search_sql/student_exact.sql"),
            include_str!("search_sql/student_prefix.sql"),
            include_str!("search_sql/student_fuzzy_word.sql"),
            include_str!("search_sql/student_fuzzy_phrase.sql"),
        )
    } else {
        (EXACT, PREFIX, FUZZY_WORD, FUZZY)
    };
    if q.trim().is_empty() {
        return Ok(vec![]);
    }
    let normalized = normalize_headword(q)
        .map_err(|_| AppError::bad_request(ErrorCode::InvalidQuery, "请输入 1–200 字的词条名称"))?;
    let mut hits = Vec::with_capacity(LIMIT);
    append(tx, exact, &normalized.key, &mut hits, exclude).await?;
    // Short words remain searchable, without broad prefix/fuzzy scans for a/an.
    if normalized.key.chars().count() >= 3 && hits.len() < LIMIT {
        if let Some(end) = prefix_end(&normalized.key) {
            while hits.len() < LIMIT {
                let found: Vec<(Uuid, Uuid)> = sqlx::query_as(prefix)
                    .bind(&normalized.key)
                    .bind(exclusions(&hits, exclude))
                    .bind((LIMIT - hits.len()) as i64)
                    .bind(&end)
                    .fetch_all(&mut **tx)
                    .await
                    .map_err(AppError::internal)?;
                if found.is_empty() {
                    break;
                }
                for hit in found {
                    if !hits.iter().any(|(id, _)| *id == hit.0) {
                        hits.push(hit);
                    }
                }
            }
        }
        if hits.len() < LIMIT && NormalizedHeadword::parse(q).is_ok() {
            sqlx::query("SELECT set_config('pg_trgm.similarity_threshold','0.3',true)")
                .execute(&mut **tx)
                .await
                .map_err(AppError::internal)?;
            if normalized.key.contains(' ') && normalized.key.chars().count() >= 8 {
                while hits.len() < LIMIT {
                    let before = hits.len();
                    append(tx, fuzzy_phrase, &normalized.display, &mut hits, exclude).await?;
                    if hits.len() == before {
                        break;
                    }
                }
            } else {
                append(tx, fuzzy_word, &normalized.display, &mut hits, exclude).await?;
            }
        }
    }
    // Serialize with archive/publication writers only for the selected entries.
    // Save still checks availability/revision; autocomplete cannot reserve a word.
    let ids = exclusions(&hits, None);
    if wordlist {
        published::lock_learning_entries(tx, &ids).await?;
    } else {
        published::lock_entries_in(tx, &ids).await?;
    }
    let items: Vec<EntrySuggestionSummary> = sqlx::query_as(SUMMARY)
        .bind(ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    Ok(items
        .into_iter()
        .filter(|r| hits.contains(&(r.entry_id, r.publication_id)))
        .collect())
}

// The exclusive C-collation bound works for Unicode and treats %/_ literally.
fn prefix_end(value: &str) -> Option<String> {
    let mut result = value.to_owned();
    while let Some(last) = result.pop() {
        let mut next = u32::from(last) + 1;
        while next <= u32::from(char::MAX) {
            if let Some(c) = char::from_u32(next) {
                result.push(c);
                return Some(result);
            }
            next += 1;
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::prefix_end;
    #[test]
    fn unicode_prefix_bounds() {
        assert_eq!(prefix_end("app"), Some("apq".into()));
        assert_eq!(prefix_end("a\u{d7ff}"), Some("a\u{e000}".into()));
        assert_eq!(prefix_end("a\u{10ffff}"), Some("b".into()));
        assert_eq!(prefix_end("\u{10ffff}"), None);
        assert_eq!(prefix_end("app%"), Some("app&".into()));
    }
}
