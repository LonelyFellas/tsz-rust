use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::Utc;
use sqlx::PgConnection;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use super::model::{NextDraft, NextEquivalentScore};
use crate::error::{AppError, ErrorCode};

fn invalid(message: &str) -> AppError {
    AppError::unprocessable(ErrorCode::InvalidRequestBody, message)
}

pub(super) fn key(text: &str) -> String {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
        .nfkc()
        .collect::<String>()
        .to_lowercase()
}

pub(super) fn score_units(value: &str) -> Option<u16> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, "0"));
    if whole.is_empty()
        || whole.len() > 2
        || !whole.bytes().all(|c| c.is_ascii_digit())
        || fraction.len() != 1
        || !fraction.bytes().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let units = whole.parse::<u16>().ok()? * 10 + fraction.parse::<u16>().ok()?;
    (1..=999).contains(&units).then_some(units)
}

pub(super) fn has_metadata(draft: &NextDraft) -> bool {
    draft.senses.iter().any(|sense| {
        !sense.no_equivalents.is_empty()
            || sense.equivalents.iter().any(|row| row.metadata.is_some())
    })
}

pub(super) fn strip(draft: &mut NextDraft) {
    for sense in &mut draft.senses {
        sense.no_equivalents.clear();
        for row in &mut sense.equivalents {
            row.metadata = None;
        }
    }
}

/// Preserve fields hidden from an older client by stable identity, never by text.
pub(super) fn preserve(draft: &mut NextDraft, previous: &NextDraft) {
    for sense in &mut draft.senses {
        let Some(saved) = previous.senses.iter().find(|row| row.id == sense.id) else {
            continue;
        };
        sense.no_equivalents = saved
            .no_equivalents
            .iter()
            .filter(|locale| !sense.equivalents.iter().any(|row| &row.locale == *locale))
            .cloned()
            .collect();
        for row in &mut sense.equivalents {
            if let Some(saved) = saved.equivalents.iter().find(|saved| saved.id == row.id) {
                row.metadata = saved.metadata.clone();
            }
        }
    }
}

pub(super) async fn prepare(
    conn: &mut PgConnection,
    draft: &mut NextDraft,
    previous: &NextDraft,
    negotiated: bool,
) -> Result<BTreeSet<Uuid>, AppError> {
    if !negotiated {
        preserve(draft, previous);
    }
    let mut refs = BTreeMap::new();
    let now = Utc::now();
    for sense in &mut draft.senses {
        let mut absent = HashSet::new();
        for locale in &sense.no_equivalents {
            super::language::require_medium(locale)?;
            if !absent.insert(locale) || sense.equivalents.iter().any(|row| &row.locale == locale) {
                return Err(invalid("无译词声明不能重复或与已有译词并存"));
            }
        }
        let mut texts = HashSet::new();
        for row in &mut sense.equivalents {
            if previous
                .senses
                .iter()
                .find(|saved| saved.id == sense.id)
                .and_then(|saved| saved.equivalents.iter().find(|saved| saved.id == row.id))
                .is_some_and(|saved| saved.locale != row.locale)
            {
                return Err(invalid("已有译词不能更换媒介语言，请新建译词"));
            }
            if negotiated {
                row.text = row
                    .text
                    .trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
                    .to_owned();
                super::language::text(&row.text, 200)?;
                if !texts.insert((row.locale.clone(), key(&row.text))) {
                    return Err(invalid("同一词义与媒介语言内的译词不能重复"));
                }
            }
            let Some(metadata) = &mut row.metadata else {
                continue;
            };
            for (ids, kind) in [
                (&metadata.register_ids, "registers"),
                (&metadata.domain_ids, "domains"),
            ] {
                if ids.len() > 1000 || ids.iter().collect::<HashSet<_>>().len() != ids.len() {
                    return Err(invalid("标签重复或数量超出限制"));
                }
                for id in ids {
                    if id.is_nil()
                        || refs
                            .insert(*id, kind)
                            .is_some_and(|existing| existing != kind)
                    {
                        return Err(invalid("标签类别不匹配"));
                    }
                }
            }
            if metadata.commonness.len() > 1001 {
                return Err(invalid("评分领域数量超出限制"));
            }
            let mut domains = HashSet::new();
            for score in &mut metadata.commonness {
                if !domains.insert(score.domain_id) {
                    return Err(invalid("每个领域只能有一个独立评分"));
                }
                let units = score_units(&score.score)
                    .ok_or_else(|| invalid("常见度必须为 0.1–99.9，最多一位小数"))?;
                score.score = format!("{}.{:01}", units / 10, units % 10);
                if let Some(id) = score.domain_id
                    && (id.is_nil()
                        || refs
                            .insert(id, "domains")
                            .is_some_and(|existing| existing != "domains"))
                {
                    return Err(invalid("评分领域无效"));
                }
                let saved: Option<&NextEquivalentScore> = previous
                    .senses
                    .iter()
                    .find(|saved| saved.id == sense.id)
                    .and_then(|saved| saved.equivalents.iter().find(|saved| saved.id == row.id))
                    .and_then(|saved| saved.metadata.as_ref())
                    .and_then(|saved| {
                        saved.commonness.iter().find(|saved| {
                            saved.domain_id == score.domain_id && saved.score == score.score
                        })
                    });
                score.assessed_at = saved.and_then(|saved| saved.assessed_at).or(Some(now));
            }
        }
    }
    // Deterministic lock order; held until snapshot + reference insert commit.
    for (id, kind) in &refs {
        let actual = sqlx::query_scalar::<_, String>(
            "SELECT kind FROM catalog.lexicon_taxonomies WHERE id=$1 FOR KEY SHARE",
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(AppError::internal)?;
        if actual.as_deref() != Some(kind) {
            return Err(invalid("语体或行业配置不存在或类别不匹配，请重新加载目录"));
        }
    }
    Ok(refs.into_keys().collect())
}

pub(super) async fn persist_refs(
    conn: &mut PgConnection,
    entry_id: Uuid,
    revision: i64,
    refs: BTreeSet<Uuid>,
) -> Result<(), AppError> {
    for taxonomy_id in refs {
        sqlx::query("INSERT INTO lexicon_next.equivalent_taxonomy_refs(entry_id,revision,taxonomy_id) VALUES($1,$2,$3)")
            .bind(entry_id).bind(revision).bind(taxonomy_id).execute(&mut *conn).await.map_err(AppError::internal)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_scores_and_unicode_deduplication() {
        for (value, units) in [("0.1", 1), ("95", 950), ("99.9", 999)] {
            assert_eq!(score_units(value), Some(units));
        }
        for value in ["0", "100", "-1", "0.01", "95.00", "NaN", "1e1", "1.", ".1"] {
            assert_eq!(score_units(value), None, "{value}");
        }
        assert_eq!(key(" ＪＯＢ "), key("job"));
        assert_eq!(key("ΟΣ"), key("ος"));
        assert_ne!(key("工作"), key("职业"));
    }
}
