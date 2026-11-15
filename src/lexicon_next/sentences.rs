//! Independent, locale-scoped sentence drafts; never resolves legacy sentence identities.
use super::{audio, language, model::NextDraft, sentence_details, sentence_model::*};
use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiJson, ApiPath, ApiQuery},
    error::{AppError, ErrorCode},
    state::AppState,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header::VARY},
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder, Row};
use std::collections::HashSet;
use uuid::Uuid;

const DETAILS_HEADER: &str = "x-tsz-next-sentence-details";

pub(super) fn supports_details(headers: &HeaderMap) -> bool {
    headers
        .get(DETAILS_HEADER)
        .is_some_and(|value| value == "v1")
}
fn for_client(mut sentence: NextSentence, headers: &HeaderMap) -> NextSentence {
    if !supports_details(headers) {
        for variant in &mut sentence.draft.variants {
            variant.metadata = None;
            for translation in &mut variant.translations {
                translation.metadata = None;
            }
        }
    }
    sentence
}
fn require_details(
    variant: &NextSentenceVariant,
    medium: &str,
    headers: &HeaderMap,
) -> Result<(), AppError> {
    if !supports_details(headers) && sentence_details::has_details(variant, medium) {
        return Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "例句包含新版自由度、标签或来源，请刷新页面后编辑；当前输入已保留",
        ));
    }
    Ok(())
}

const LEVELS: &[&str] = &["A1", "A2", "B1", "B2", "C1", "C2"];
const SELECT: &str = "SELECT s.id,s.target_language,s.owner_id,s.revision,s.updated_at,s.document,(SELECT count(*) FROM lexicon_next.sentence_references r WHERE r.sentence_id=s.id) AS reference_count FROM lexicon_next.sentences s";

pub(super) fn decode(row: sqlx::postgres::PgRow) -> Result<NextSentence, AppError> {
    Ok(NextSentence {
        id: row.get("id"),
        target_language: row.get("target_language"),
        owner_id: row.get("owner_id"),
        revision: row.get("revision"),
        updated_at: row.get("updated_at"),
        reference_count: row.get("reference_count"),
        draft: serde_json::from_value(row.get("document")).map_err(AppError::internal)?,
    })
}

async fn get(pool: &PgPool, id: Uuid) -> Result<NextSentence, AppError> {
    let mut query = QueryBuilder::<Postgres>::new(SELECT);
    query.push(" WHERE s.id=").push_bind(id);
    let row = query
        .build()
        .fetch_optional(pool)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("例句不存在"))?;
    decode(row)
}

fn target(locale: &str) -> Result<(), AppError> {
    if matches!(locale, "en-GB" | "en-US") {
        Ok(())
    } else {
        Err(language::unavailable())
    }
}

pub(super) fn validate(id: Uuid, draft: &NextSentenceDraft) -> Result<(), AppError> {
    if draft.schema_version != 1 || draft.variants.is_empty() {
        return Err(language::invalid());
    }
    let mut ids = HashSet::new();
    language::identity(&mut ids, id)?;
    let mut locales = HashSet::new();
    for variant in &draft.variants {
        target(&variant.locale)?;
        language::identity(&mut ids, variant.id)?;
        if !locales.insert(&variant.locale)
            || !LEVELS.contains(&variant.level.as_str())
            || variant.content.text().trim().is_empty()
            || !crate::lexicon::rich_text::is_valid_native(&variant.content)
            || variant.audio_assets.len() > 8
            || variant
                .audio_assets
                .iter()
                .map(|a| a.id)
                .collect::<HashSet<_>>()
                .len()
                != variant.audio_assets.len()
        {
            return Err(language::invalid());
        }
        let mut issues = Vec::new();
        crate::lexicon::v3_contract::validate_voice_profile(
            variant.voice_profile.as_ref(),
            variant.id,
            &mut issues,
        );
        if !issues.is_empty()
            || variant
                .voice_profile
                .as_ref()
                .and_then(|p| p.dubbing.as_ref())
                .is_some_and(|p| p.area != crate::lexicon::dto::DubbingAreaV3::Example)
        {
            return Err(language::invalid());
        }
        for translation in &variant.translations {
            language::identity(&mut ids, translation.id)?;
            language::require_medium(&translation.locale)?;
            if !crate::lexicon::rich_text::is_valid_native(&translation.content) {
                return Err(language::invalid());
            }
        }
    }
    Ok(())
}

async fn node(
    conn: &mut PgConnection,
    id: Uuid,
    sentence: Uuid,
    kind: &str,
    locale: &str,
    parent: Option<Uuid>,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO lexicon_next.sentence_nodes(id,sentence_id,kind,locale,parent_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT(id) DO NOTHING").bind(id).bind(sentence).bind(kind).bind(locale).bind(parent).execute(&mut *conn).await.map_err(AppError::internal)?;
    let existing: (Uuid, String, String, Option<Uuid>) = sqlx::query_as(
        "SELECT sentence_id,kind,locale,parent_id FROM lexicon_next.sentence_nodes WHERE id=$1",
    )
    .bind(id)
    .fetch_one(conn)
    .await
    .map_err(AppError::internal)?;
    if existing != (sentence, kind.into(), locale.into(), parent) {
        return Err(AppError::conflict(
            ErrorCode::StableNodeIdChanged,
            None,
            "例句节点已属于其他资源、语言或位置",
        ));
    }
    Ok(())
}

pub(super) fn scoped_variant(
    locale: &str,
    medium: &str,
    variant: &NextSentenceVariant,
) -> Result<(), AppError> {
    target(locale)?;
    language::require_medium(medium)?;
    if variant.locale != locale || variant.translations.iter().any(|v| v.locale != medium) {
        return Err(language::invalid());
    }
    Ok(())
}

pub(super) async fn prepare(
    conn: &mut PgConnection,
    actor: Uuid,
    id: Uuid,
    draft: &mut NextSentenceDraft,
    changed_locale: &str,
) -> Result<(), AppError> {
    sentence_details::persist_taxonomies(conn, id, draft).await?;
    for variant in &draft.variants {
        node(conn, variant.id, id, "variant", &variant.locale, None).await?;
        for translation in &variant.translations {
            node(
                conn,
                translation.id,
                id,
                "translation",
                &translation.locale,
                Some(variant.id),
            )
            .await?;
        }
    }
    let mut serialized = serde_json::to_value(&draft).map_err(AppError::internal)?;
    let mut bindings = serde_json::to_value(
        draft
            .variants
            .iter()
            .filter(|v| v.locale == changed_locale)
            .collect::<Vec<_>>(),
    )
    .map_err(AppError::internal)?;
    for variant in bindings.as_array_mut().expect("typed sentence variants") {
        variant["dialect"] = if variant["locale"] == "en-GB" {
            "uk".into()
        } else {
            "us".into()
        };
    }
    crate::speech::generated::validate_bindings(conn, &bindings).await?;
    audio::persist_value(conn, actor, audio::Owner::Sentence(id), &mut serialized).await?;
    *draft = serde_json::from_value(serialized).map_err(AppError::internal)?;
    for variant in &draft.variants {
        let accent = if variant.locale == "en-GB" {
            crate::speech::locale::AccentGroup::Uk
        } else {
            crate::speech::locale::AccentGroup::Us
        };
        if variant.voice_profile.as_ref().is_some_and(|profile| {
            profile.voices.iter().any(|voice| {
                voice.enabled && voice.accent_group().is_some_and(|group| group != accent)
            })
        }) || variant
            .audio_assets
            .iter()
            .any(|asset| asset.locale.accent_group() != accent)
        {
            return Err(language::invalid());
        }
    }
    Ok(())
}

pub(super) fn merge_variant(
    previous: &NextSentenceVariant,
    next: &mut NextSentenceVariant,
    medium: &str,
) -> Result<(), AppError> {
    if previous.metadata.is_some() && next.metadata.is_none()
        || previous.translations.iter().any(|row| {
            row.locale == medium
                && row.metadata.is_some()
                && next
                    .translations
                    .iter()
                    .any(|next| next.id == row.id && next.metadata.is_none())
        })
    {
        return Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "例句扩展字段缺失，请重新加载后保存",
        ));
    }
    if previous.id != next.id {
        return Err(AppError::conflict(
            ErrorCode::StableNodeIdChanged,
            None,
            "同一语言的例句节点身份不能变更",
        ));
    }
    next.translations.extend(
        previous
            .translations
            .iter()
            .filter(|v| v.locale != medium)
            .cloned(),
    );
    Ok(())
}

#[utoipa::path(get,path="/api/v1/admin/lexicon-next/sentences/{id}",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("x-tsz-next-sentence-details"=Option<String>, Header, description="v1 negotiates sentence_details: freedom, taxonomy and manual source metadata. Omission hides metadata; legacy writes to enhanced content are refused.")),responses((status=200,body=NextSentence),(status=401),(status=403),(status=404)))]
pub async fn read(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("sentences.access")?;
    Ok((
        [(VARY, DETAILS_HEADER)],
        Json(for_client(get(&state.pool, id).await?, &headers)),
    )
        .into_response())
}

#[utoipa::path(post,path="/api/v1/admin/lexicon-next/sentences",tag="admin-lexicon-next",security(("bearer_auth"=[])),request_body=CreateNextSentence,params(("x-tsz-next-sentence-details"=Option<String>, Header, description="v1 negotiates sentence_details: freedom, taxonomy and manual source metadata. Omission hides metadata; legacy writes to enhanced content are refused.")),responses((status=201,body=NextSentence),(status=401),(status=403),(status=409),(status=422)))]
pub async fn create(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiJson(mut input): ApiJson<CreateNextSentence>,
) -> Result<Response, AppError> {
    require_details(&input.variant, &input.medium_locale, &headers)?;
    sentence_details::normalize(&mut input.variant)?;
    language::require_language(&input.target_language)?;
    scoped_variant(&input.target_locale, &input.medium_locale, &input.variant)?;
    let digest = Sha256::digest(
        serde_json::to_vec(&(
            &input.target_language,
            &input.target_locale,
            &input.medium_locale,
            &input.variant,
        ))
        .map_err(AppError::internal)?,
    )
    .to_vec();
    let mut draft = NextSentenceDraft {
        schema_version: 1,
        variants: vec![input.variant],
    };
    validate(input.id, &draft)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "sentences.create").await?;
    let foreign:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon.shared_sentences WHERE id=$1 UNION ALL SELECT 1 FROM lexicon_next.entries WHERE id=$1)").bind(input.id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    if foreign {
        return Err(AppError::conflict(
            ErrorCode::IdempotencyConflict,
            None,
            "例句身份已属于其他资源",
        ));
    }
    let inserted=sqlx::query("INSERT INTO lexicon_next.sentences(id,target_language,owner_id,revision,document,create_digest) VALUES($1,$2,$3,1,$4,$5) ON CONFLICT(id) DO NOTHING").bind(input.id).bind(&input.target_language).bind(auth.subject).bind(serde_json::to_value(&draft).map_err(AppError::internal)?).bind(&digest).execute(&mut *tx).await.map_err(AppError::internal)?.rows_affected();
    if inserted == 0 {
        let existing: (Uuid, Vec<u8>) = sqlx::query_as(
            "SELECT owner_id,create_digest FROM lexicon_next.sentences WHERE id=$1 FOR UPDATE",
        )
        .bind(input.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        if existing != (auth.subject, digest) {
            return Err(AppError::conflict(
                ErrorCode::IdempotencyConflict,
                None,
                "例句创建身份已用于其他内容",
            ));
        }
    } else {
        prepare(
            &mut tx,
            auth.subject,
            input.id,
            &mut draft,
            &input.target_locale,
        )
        .await?;
        sqlx::query("UPDATE lexicon_next.sentences SET document=$2 WHERE id=$1")
            .bind(input.id)
            .bind(serde_json::to_value(draft).map_err(AppError::internal)?)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok((
        StatusCode::CREATED,
        [(VARY, DETAILS_HEADER)],
        Json(for_client(get(&state.pool, input.id).await?, &headers)),
    )
        .into_response())
}

#[utoipa::path(put,path="/api/v1/admin/lexicon-next/sentences/{id}",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("x-tsz-next-sentence-details"=Option<String>, Header, description="v1 negotiates sentence_details: freedom, taxonomy and manual source metadata. Omission hides metadata; legacy writes to enhanced content are refused.")),request_body=SaveNextSentence,responses((status=200,body=NextSentence),(status=401),(status=403),(status=404),(status=409),(status=422)))]
pub async fn save(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(mut input): ApiJson<SaveNextSentence>,
) -> Result<Response, AppError> {
    require_details(&input.variant, &input.medium_locale, &headers)?;
    sentence_details::normalize(&mut input.variant)?;
    if input.expected_revision < 1 {
        return Err(language::invalid());
    }
    scoped_variant(&input.target_locale, &input.medium_locale, &input.variant)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "sentences.edit").await?;
    let existing: Option<(Uuid, i64, serde_json::Value)> = sqlx::query_as(
        "SELECT owner_id,revision,document FROM lexicon_next.sentences WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let (owner, revision, document) = existing.ok_or_else(|| AppError::not_found("例句不存在"))?;
    permissions::lock(&mut tx, auth.subject)
        .await?
        .require_owned_action("sentences.edit", owner)?;
    if revision != input.expected_revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("expected_revision"),
            "例句已更新，输入已保留，请重新加载后保存",
        ));
    }
    let mut draft: NextSentenceDraft =
        serde_json::from_value(document).map_err(AppError::internal)?;
    if let Some(position) = draft
        .variants
        .iter()
        .position(|v| v.locale == input.target_locale)
    {
        let previous = &draft.variants[position];
        require_details(previous, &input.medium_locale, &headers)?;
        merge_variant(previous, &mut input.variant, &input.medium_locale)?;
        draft.variants[position] = input.variant;
    } else {
        draft.variants.push(input.variant);
    }
    validate(id, &draft)?;
    prepare(&mut tx, auth.subject, id, &mut draft, &input.target_locale).await?;
    sqlx::query("UPDATE lexicon_next.sentences SET revision=revision+1,document=$2,updated_at=clock_timestamp() WHERE id=$1").bind(id).bind(serde_json::to_value(draft).map_err(AppError::internal)?).execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok((
        [(VARY, DETAILS_HEADER)],
        Json(for_client(get(&state.pool, id).await?, &headers)),
    )
        .into_response())
}

fn filter(query: &mut QueryBuilder<Postgres>, selection: &NextSentenceQuery) {
    query.push(" WHERE s.target_language=").push_bind(&selection.target_language).push(" AND EXISTS(SELECT 1 FROM jsonb_array_elements(s.document->'variants') variant WHERE variant->>'locale'=").push_bind(&selection.target_locale);
    if let Some(level) = &selection.level {
        query.push(" AND variant->>'level'=").push_bind(level);
    }
    if let Some(keyword) = selection
        .keyword
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let pattern = format!(
            "%{}%",
            keyword
                .trim()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        query.push(" AND (variant->'content'->>'text' ILIKE ").push_bind(pattern.clone()).push(" OR EXISTS(SELECT 1 FROM jsonb_array_elements(variant->'translations') translation WHERE translation->>'locale'=").push_bind(&selection.medium_locale).push(" AND translation->'content'->>'text' ILIKE ").push_bind(pattern).push("))");
    }
    query.push(")");
}

#[utoipa::path(get,path="/api/v1/admin/lexicon-next/sentences",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(NextSentenceQuery),responses((status=200,body=NextSentenceList),(status=401),(status=403),(status=422)))]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<NextSentenceQuery>,
) -> Result<Json<NextSentenceList>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("sentences.access")?;
    language::require_language(&query.target_language)?;
    target(&query.target_locale)?;
    language::require_medium(&query.medium_locale)?;
    let page = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(20);
    if page < 1
        || !(1..=100).contains(&size)
        || query
            .keyword
            .as_ref()
            .is_some_and(|v| v.chars().count() > 200)
        || query
            .level
            .as_ref()
            .is_some_and(|v| !LEVELS.contains(&v.as_str()))
    {
        return Err(language::invalid());
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let mut count = QueryBuilder::new("SELECT count(*) FROM lexicon_next.sentences s");
    filter(&mut count, &query);
    let total: i64 = count
        .build_query_scalar()
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let mut rows = QueryBuilder::new(SELECT);
    filter(&mut rows, &query);
    rows.push(" ORDER BY s.updated_at DESC,s.id DESC LIMIT ")
        .push_bind(size)
        .push(" OFFSET ")
        .push_bind(i64::from(page - 1) * i64::from(size));
    let mut items = Vec::new();
    for row in rows
        .build()
        .fetch_all(&mut *tx)
        .await
        .map_err(AppError::internal)?
    {
        let sentence = decode(row)?;
        let variant = sentence
            .draft
            .variants
            .into_iter()
            .find(|v| v.locale == query.target_locale)
            .ok_or_else(language::invalid)?;
        items.push(NextSentenceListItem {
            id: sentence.id,
            owner_id: sentence.owner_id,
            revision: sentence.revision,
            updated_at: sentence.updated_at,
            reference_count: sentence.reference_count,
            locale: variant.locale,
            level: variant.level,
            content: variant.content,
            translation_count: variant
                .translations
                .iter()
                .filter(|v| v.locale == query.medium_locale && !v.content.text().trim().is_empty())
                .count(),
        });
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(NextSentenceList {
        items,
        pagination: crate::api::page_meta(page as u32, size as u32, total),
    }))
}

pub(super) fn preserve(
    draft: &mut NextDraft,
    previous: &NextDraft,
    supported: bool,
) -> Result<(), AppError> {
    for sense in &mut draft.senses {
        let old = previous
            .senses
            .iter()
            .find(|v| v.id == sense.id)
            .and_then(|v| v.sentence_ids.as_ref());
        if !supported {
            sense.sentence_ids = old.cloned();
        } else if old.is_some() && sense.sentence_ids.is_none() {
            return Err(AppError::unprocessable(
                ErrorCode::InvalidRequestBody,
                "例句引用缺失，请重新打开词条后保存",
            ));
        }
        if let Some(ids) = &sense.sentence_ids
            && (ids.iter().any(Uuid::is_nil)
                || ids.iter().collect::<HashSet<_>>().len() != ids.len())
        {
            return Err(language::invalid());
        }
    }
    Ok(())
}

pub(super) async fn persist_references(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    actor: Uuid,
    entry: Uuid,
    language: &str,
    draft: &NextDraft,
    previous: &NextDraft,
) -> Result<(), AppError> {
    let mut ids = draft
        .senses
        .iter()
        .flat_map(|s| s.sentence_ids.iter().flatten().copied())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    let previous_references = previous
        .senses
        .iter()
        .flat_map(|sense| {
            sense
                .sentence_ids
                .iter()
                .flatten()
                .map(move |id| (sense.id, *id))
        })
        .collect::<HashSet<_>>();
    if draft.senses.iter().any(|sense| {
        sense
            .sentence_ids
            .iter()
            .flatten()
            .any(|id| !previous_references.contains(&(sense.id, *id)))
    }) {
        permissions::lock(tx, actor)
            .await?
            .require("sentences.access")?;
    }
    let conn = &mut **tx;
    let existing:Vec<(Uuid,String)>=sqlx::query_as("SELECT id,target_language FROM lexicon_next.sentences WHERE id=ANY($1) ORDER BY id FOR SHARE").bind(&ids).fetch_all(&mut *conn).await.map_err(AppError::internal)?;
    if existing.len() != ids.len() || existing.iter().any(|(_, value)| value != language) {
        return Err(language::invalid());
    }
    sqlx::query("DELETE FROM lexicon_next.sentence_references WHERE entry_id=$1")
        .bind(entry)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    for sense in &draft.senses {
        for (position, id) in sense.sentence_ids.iter().flatten().enumerate() {
            sqlx::query("INSERT INTO lexicon_next.sentence_references(entry_id,sense_id,sentence_id,position) VALUES($1,$2,$3,$4)").bind(entry).bind(sense.id).bind(id).bind(position as i32).execute(&mut *conn).await.map_err(AppError::internal)?;
        }
    }
    Ok(())
}
