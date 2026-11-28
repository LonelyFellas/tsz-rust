use super::{language, list_model::NextWorkspaceEntry, model::NextEntryKind};
use crate::{error::AppError, lexicon::normalization::NormalizedHeadword};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, QueryBuilder};
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextEntryMatchInput {
    pub target_language: String,
    pub medium_locale: Option<String>,
    pub surface: String,
    #[schema(minimum = 1)]
    pub page: Option<u32>,
    #[schema(minimum = 1, maximum = 100)]
    pub page_size: Option<u32>,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextEntryMatches {
    pub surface: String,
    pub items: Vec<NextWorkspaceEntry>,
    pub pagination: crate::api::PaginationMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation_items: Option<Vec<NextWorkspaceEntry>>,
}

// Same effective spellings as the list: current forms, or the input headword
// only while the draft has no forms. EXISTS keeps one result per entry even
// when several locales match. Match across kinds, including multiword forms.
const MATCH: &str = r#" AND EXISTS (
    SELECT 1 FROM (
        SELECT f.text AS target FROM lexicon_next.forms f
        JOIN lexicon_next.nodes n ON n.id=f.id WHERE n.entry_id=e.id
        UNION ALL SELECT e.headword WHERE NOT EXISTS (
            SELECT 1 FROM lexicon_next.forms f JOIN lexicon_next.nodes n ON n.id=f.id WHERE n.entry_id=e.id
        )
    ) surfaces WHERE
        lower(translate(btrim(regexp_replace(normalize(target,NFKC) COLLATE "und-x-icu",
            '[[:space:]]+',' ','g')), '‘’ʼ‐‑‒–—−',repeat(chr(39),3)||repeat('-',6)) COLLATE "und-x-icu")
            = lower("#;

fn filter(sql: &mut QueryBuilder<Postgres>, surface: &str, language: &str, medium: Option<&str>) {
    sql.push(" WHERE e.status<>'archived' AND e.target_language=")
        .push_bind(language)
        .push(MATCH)
        .push_bind(surface)
        .push(" COLLATE \"und-x-icu\"))");
    if let Some(locale) = medium {
        sql.push(" AND e.medium_locale=").push_bind(locale);
    }
}

pub async fn find(
    pool: &PgPool,
    input: NextEntryMatchInput,
    include_annotation_rule: bool,
    annotation_kind: Option<NextEntryKind>,
    component_words: bool,
) -> Result<NextEntryMatches, AppError> {
    language::require_language(&input.target_language)?;
    if let Some(locale) = &input.medium_locale {
        language::require_medium(locale)?;
    }
    let surface = NormalizedHeadword::parse(&input.surface).map_err(|_| language::invalid())?;
    let page = input.page.unwrap_or(1);
    let size = input.page_size.unwrap_or(20);
    if page == 0 || !(1..=100).contains(&size) {
        return Err(language::invalid());
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let mut count = QueryBuilder::new("SELECT count(*) FROM lexicon_next.entries e");
    filter(
        &mut count,
        &surface.key,
        &input.target_language,
        input.medium_locale.as_deref(),
    );
    if component_words {
        count.push(" AND e.kind='word'");
    }
    let total = count
        .build_query_scalar()
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let mut pagination = crate::api::page_meta(page, size, total);
    pagination.page = i64::from(page).min(pagination.total_pages.max(1)) as u32;
    let mut rows = super::workspace::select(input.medium_locale.as_deref());
    filter(
        &mut rows,
        &surface.key,
        &input.target_language,
        input.medium_locale.as_deref(),
    );
    if component_words {
        rows.push(" AND e.kind='word'");
    }
    rows.push(" ORDER BY e.updated_at DESC,e.id DESC LIMIT ")
        .push_bind(i64::from(size))
        .push(" OFFSET ")
        .push_bind(i64::from(pagination.page - 1) * i64::from(size));
    let items = rows
        .build_query_as()
        .fetch_all(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let annotation_items = if include_annotation_rule {
        let mut rows = super::workspace::select(input.medium_locale.as_deref());
        rows.push(" WHERE e.status<>'archived' AND e.target_language=")
            .push_bind(&input.target_language)
            .push(
                r#" AND lower(translate(btrim(regexp_replace(normalize(e.headword,NFKC) COLLATE "und-x-icu",
                    '[[:space:]]+',' ','g')), '‘’ʼ‐‑‒–—−',repeat(chr(39),3)||repeat('-',6)) COLLATE "und-x-icu")
                    = lower("#,
            )
            .push_bind(&surface.key)
            .push(" COLLATE \"und-x-icu\")");
        if let Some(locale) = &input.medium_locale {
            rows.push(" AND e.medium_locale=").push_bind(locale);
        }
        if let Some(kind) = &annotation_kind {
            rows.push(" AND e.kind=").push_bind(match kind {
                NextEntryKind::Word => "word",
                NextEntryKind::Phrase => "phrase",
            });
        }
        rows.push(" ORDER BY e.updated_at DESC,e.id DESC LIMIT 100");
        Some(
            rows.build_query_as()
                .fetch_all(&mut *tx)
                .await
                .map_err(AppError::internal)?,
        )
    } else {
        None
    };
    let annotation_required = annotation_items.as_ref().map(|items| !items.is_empty());
    tx.commit().await.map_err(AppError::internal)?;
    Ok(NextEntryMatches {
        surface: surface.display,
        items,
        pagination,
        annotation_required,
        annotation_items,
    })
}
