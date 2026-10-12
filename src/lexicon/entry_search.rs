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
#[derive(sqlx::FromRow)]
pub(crate) struct EntrySuggestionSummary {
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
fn exclusions(ids: &[Uuid], exclude: Option<Uuid>) -> Vec<Uuid> {
    ids.iter().copied().chain(exclude).collect()
}
async fn append(
    tx: &mut Tx<'_>,
    sql: &'static str,
    query: &str,
    ids: &mut Vec<Uuid>,
    exclude: Option<Uuid>,
) -> Result<(), AppError> {
    let found: Vec<Uuid> = sqlx::query_scalar(sql)
        .bind(query)
        .bind(exclusions(ids, exclude))
        .bind((LIMIT - ids.len()) as i64)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    ids.extend(found);
    Ok(())
}
pub(crate) async fn search(
    tx: &mut Tx<'_>,
    q: &str,
    exclude: Option<Uuid>,
) -> Result<Vec<EntrySuggestionSummary>, AppError> {
    if q.trim().is_empty() {
        return Ok(vec![]);
    }
    let normalized = normalize_headword(q)
        .map_err(|_| AppError::bad_request(ErrorCode::InvalidQuery, "请输入 1–200 字的词汇"))?;
    let mut ids = Vec::with_capacity(LIMIT);
    append(tx, EXACT, &normalized.key, &mut ids, exclude).await?;
    // Short words remain searchable, without broad prefix/fuzzy scans for a/an.
    if normalized.key.chars().count() >= 3 && ids.len() < LIMIT {
        if let Some(end) = prefix_end(&normalized.key) {
            while ids.len() < LIMIT {
                let found: Vec<Uuid> = sqlx::query_scalar(PREFIX)
                    .bind(&normalized.key)
                    .bind(exclusions(&ids, exclude))
                    .bind((LIMIT - ids.len()) as i64)
                    .bind(&end)
                    .fetch_all(&mut **tx)
                    .await
                    .map_err(AppError::internal)?;
                if found.is_empty() {
                    break;
                }
                for id in found {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
        if ids.len() < LIMIT && NormalizedHeadword::parse(q).is_ok() {
            sqlx::query("SELECT set_config('pg_trgm.similarity_threshold','0.3',true)")
                .execute(&mut **tx)
                .await
                .map_err(AppError::internal)?;
            if normalized.key.contains(' ') && normalized.key.chars().count() >= 8 {
                while ids.len() < LIMIT {
                    let before = ids.len();
                    append(tx, FUZZY, &normalized.display, &mut ids, exclude).await?;
                    if ids.len() == before {
                        break;
                    }
                }
            } else {
                append(tx, FUZZY_WORD, &normalized.display, &mut ids, exclude).await?;
            }
        }
    }
    // Serialize with archive/publication writers only for the selected entries.
    // Save still checks availability/revision; autocomplete cannot reserve a word.
    published::lock_entries_in(tx, &ids).await?;
    let items = sqlx::query_as(SUMMARY)
        .bind(ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    Ok(items)
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
