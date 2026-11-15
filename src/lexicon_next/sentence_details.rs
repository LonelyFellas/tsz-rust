use std::collections::{BTreeMap, BTreeSet, HashSet};

use sqlx::PgConnection;
use uuid::Uuid;

use super::sentence_model::*;
use crate::error::{AppError, ErrorCode};

fn invalid(message: &str) -> AppError {
    AppError::unprocessable(ErrorCode::InvalidRequestBody, message)
}

pub(super) fn category(value: f64) -> Result<NextSentenceTranslationStyle, AppError> {
    let units = value * 10.0;
    if !value.is_finite() || !(0.0..=100.0).contains(&value) || (units - units.round()).abs() > 1e-8
    {
        return Err(invalid("翻译自由度须为 0–100%，最多一位小数"));
    }
    Ok(if units.round() <= 333.0 {
        NextSentenceTranslationStyle::WordForWord
    } else if units.round() <= 666.0 {
        NextSentenceTranslationStyle::BalancedFluency
    } else {
        NextSentenceTranslationStyle::AdaptedCreation
    })
}

fn sources(rows: &[NextSentenceSource]) -> Result<(), AppError> {
    if rows.len() > 20 {
        return Err(invalid("每条内容最多登记 20 个来源"));
    }
    let mut ids = HashSet::new();
    for row in rows {
        if row.id.is_nil() || !ids.insert(row.id) {
            return Err(invalid("来源身份无效或重复"));
        }
        for (value, max) in [(&row.title, 500), (&row.url, 2000), (&row.locator, 2000)] {
            if value.chars().count() > max || value.chars().any(char::is_control) {
                return Err(invalid("来源字段过长或包含控制字符"));
            }
        }
        if !row.url.is_empty() {
            let url = reqwest::Url::parse(&row.url)
                .map_err(|_| invalid("来源链接须为有效的 http/https 地址"))?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.host_str().is_none()
            {
                return Err(invalid("来源链接须为不含凭据的 http/https 地址"));
            }
        }
        if matches!(
            row.source_type,
            NextSentenceSourceType::Dictionary
                | NextSentenceSourceType::Web
                | NextSentenceSourceType::Adapted
        ) && (row.title.trim().is_empty() || row.url.is_empty() && row.locator.trim().is_empty())
        {
            return Err(invalid("引用或改写须登记来源名称及网页链接或具体定位"));
        }
    }
    Ok(())
}

pub(super) fn has_details(variant: &NextSentenceVariant, medium: &str) -> bool {
    variant.metadata.is_some()
        || variant
            .translations
            .iter()
            .any(|row| row.locale == medium && row.metadata.is_some())
}

pub(super) fn normalize(variant: &mut NextSentenceVariant) -> Result<(), AppError> {
    if let Some(metadata) = &variant.metadata {
        sources(&metadata.sources)?;
        for ids in [&metadata.register_ids, &metadata.domain_ids] {
            if ids.len() > 1000
                || ids.iter().any(Uuid::is_nil)
                || ids.iter().collect::<HashSet<_>>().len() != ids.len()
            {
                return Err(invalid("语体或领域标签无效、重复或过多"));
            }
        }
    }
    for row in &mut variant.translations {
        if let Some(metadata) = &row.metadata {
            sources(&metadata.sources)?;
            if row.content.text().trim().is_empty() {
                return Err(invalid("译文不能为空；不需要的译文请移除"));
            }
            if let Some(value) = metadata.freedom {
                row.style = category(value)?;
            }
        }
    }
    Ok(())
}

pub(super) async fn persist_taxonomies(
    conn: &mut PgConnection,
    sentence: Uuid,
    draft: &NextSentenceDraft,
) -> Result<(), AppError> {
    let mut refs = BTreeMap::<Uuid, BTreeSet<&str>>::new();
    for variant in &draft.variants {
        if let Some(metadata) = &variant.metadata {
            for (ids, kind) in [
                (&metadata.register_ids, "registers"),
                (&metadata.domain_ids, "domains"),
            ] {
                for id in ids {
                    refs.entry(*id).or_default().insert(kind);
                }
            }
        }
    }
    for (id, kinds) in &refs {
        let kind = sqlx::query_scalar::<_, String>(
            "SELECT kind FROM catalog.lexicon_taxonomies WHERE id=$1 FOR KEY SHARE",
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(AppError::internal)?;
        if kinds.len() != 1 || !kind.as_deref().is_some_and(|kind| kinds.contains(kind)) {
            return Err(invalid("语体或领域不存在或类别不匹配，请重新加载目录"));
        }
    }
    sqlx::query("DELETE FROM lexicon_next.sentence_taxonomy_refs WHERE sentence_id=$1")
        .bind(sentence)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    for id in refs.into_keys() {
        sqlx::query("INSERT INTO lexicon_next.sentence_taxonomy_refs(sentence_id,taxonomy_id) VALUES($1,$2)")
            .bind(sentence).bind(id).execute(&mut *conn).await.map_err(AppError::internal)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn freedom_is_exact_tenths_with_three_continuous_categories() {
        for (value, expected) in [
            (0.0, NextSentenceTranslationStyle::WordForWord),
            (33.3, NextSentenceTranslationStyle::WordForWord),
            (33.4, NextSentenceTranslationStyle::BalancedFluency),
            (66.6, NextSentenceTranslationStyle::BalancedFluency),
            (66.7, NextSentenceTranslationStyle::AdaptedCreation),
            (100.0, NextSentenceTranslationStyle::AdaptedCreation),
        ] {
            assert_eq!(category(value).unwrap(), expected);
        }
        for value in [-0.1, 100.1, 33.33, f64::NAN, f64::INFINITY] {
            assert!(category(value).is_err());
        }
    }
}
