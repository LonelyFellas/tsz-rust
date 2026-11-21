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

fn valid_source_segments(text: &str, associations: &[NextSentenceAnnotation]) -> bool {
    if associations.len() > 100 {
        return false;
    }
    let chars: Vec<_> = text.chars().collect();
    let tokens = crate::lexicon::text_tokenization::tokenize(text);
    let mut ids = HashSet::new();
    let mut occupied = HashSet::new();
    associations.iter().all(|association| {
        if association.id.is_nil()
            || !ids.insert(association.id)
            || association.source_segments.is_empty()
            || association.source_segments.len() > 20
        {
            return false;
        }
        let mut previous_end = 0;
        for segment in &association.source_segments {
            let start = segment.start;
            let end = segment.end;
            if start >= end
                || start < previous_end
                || end > chars.len()
                || segment.surface.trim().is_empty()
                || !tokens.iter().any(|token| token.range.start == start)
                || !tokens.iter().any(|token| token.range.end == end)
                || chars[start..end].iter().collect::<String>() != segment.surface
                || chars[start..end]
                    .iter()
                    .any(|character| matches!(character, '\n' | '\r'))
                || (start..end).any(|index| !occupied.insert(index))
            {
                return false;
            }
            previous_end = end;
        }
        let first = association.source_segments.first().unwrap().start;
        let last = association.source_segments.last().unwrap().end;
        !chars[first..last]
            .iter()
            .any(|character| matches!(character, '\n' | '\r'))
    })
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

pub(super) async fn hydrate(
    conn: &mut PgConnection,
    sentence: &mut NextSentence,
) -> Result<(), AppError> {
    let rows = sqlx::query(
        "SELECT id,source_variant_id,source_dialect,source_segments,state,target_kind,target_source,target_word_id,target_publication_id,target_pos_id,target_base_form_id,target_sense_id,target_form_id,target_variant_id,target_dialect,pending_headword,pending_gloss FROM lexicon_next.sentence_associations WHERE sentence_id=$1 ORDER BY source_variant_id,source_start",
    )
    .bind(sentence.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    for variant in &mut sentence.draft.variants {
        let associations = rows
            .iter()
            .filter(|row| row.get::<Uuid, _>("source_variant_id") == variant.id)
            .map(|row| {
                let segments =
                    serde_json::from_value(row.get::<serde_json::Value, _>("source_segments"))
                        .map_err(AppError::internal)?;
                let target = match row.get::<String, _>("state").as_str() {
                    "linked" => NextSentenceTarget::Linked {
                        target_source: match row.get::<String, _>("target_source").as_str() {
                            "v3" => None,
                            "lexicon_next" => {
                                Some(crate::lexicon::dto::AssociationTargetSource::LexiconNext)
                            }
                            _ => return Err(language::invalid()),
                        },
                        target_entry_id: row.get("target_word_id"),
                        target_pos_id: row.get("target_pos_id"),
                        target_base_form_id: row.get("target_base_form_id"),
                        target_form_id: row.get("target_form_id"),
                        target_variant_id: row.get("target_variant_id"),
                        target_sense_id: row.get("target_sense_id"),
                        target_dialect: row
                            .get::<Option<String>, _>("target_dialect")
                            .map(|value| {
                                crate::lexicon::dto::Dialect::parse(&value)
                                    .ok_or_else(language::invalid)
                            })
                            .transpose()?,
                        target_publication_id: row.get("target_publication_id"),
                    },
                    "entry_only" => NextSentenceTarget::EntryOnly {
                        target_source: match row.get::<String, _>("target_source").as_str() {
                            "v3" => None,
                            "lexicon_next" => {
                                Some(crate::lexicon::dto::AssociationTargetSource::LexiconNext)
                            }
                            _ => return Err(language::invalid()),
                        },
                        target_entry_id: row.get("target_word_id"),
                    },
                    "pending" => NextSentenceTarget::Pending {
                        kind: row.get::<String, _>("target_kind"),
                        headword: row.get::<String, _>("pending_headword"),
                        gloss: row.get("pending_gloss"),
                    },
                    _ => {
                        return Err(AppError::internal(anyhow::anyhow!(
                            "invalid sentence association state"
                        )));
                    }
                };
                Ok(NextSentenceAnnotation {
                    id: row.get("id"),
                    source_dialect: row.get("source_dialect"),
                    source_segments: segments,
                    target,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        if !associations.is_empty() {
            variant.associations = Some(associations);
        }
    }
    Ok(())
}

async fn validate_target(
    conn: &mut PgConnection,
    target: &NextSentenceTarget,
) -> Result<(), AppError> {
    let (source, entry, publication, pos, sense, base_form, form, variant) = match target {
        NextSentenceTarget::Linked {
            target_source,
            target_entry_id,
            target_publication_id,
            target_pos_id,
            target_base_form_id,
            target_form_id,
            target_variant_id,
            target_sense_id,
            ..
        } => (
            target_source.as_ref(),
            Some(*target_entry_id),
            *target_publication_id,
            Some(*target_pos_id),
            Some(*target_sense_id),
            Some(*target_base_form_id),
            Some(*target_form_id),
            Some(*target_variant_id),
        ),
        NextSentenceTarget::EntryOnly {
            target_source,
            target_entry_id,
        } => (
            target_source.as_ref(),
            Some(*target_entry_id),
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        NextSentenceTarget::Pending { .. } => return Ok(()),
    };
    let next = matches!(
        source,
        Some(crate::lexicon::dto::AssociationTargetSource::LexiconNext)
    );
    let valid = if let (Some(entry), Some(publication)) = (entry, publication.filter(|_| !next)) {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon.entries e WHERE e.id=$1 AND e.kind='word') AND EXISTS(SELECT 1 FROM lexicon.entry_publications p WHERE p.id=$2 AND p.entry_id=$1) AND EXISTS(SELECT 1 FROM lexicon.entry_publication_nodes n WHERE n.publication_id=$2 AND n.entry_id=$1 AND n.node_id=$3 AND n.node_type='pos') AND EXISTS(SELECT 1 FROM lexicon.entry_publication_nodes n WHERE n.publication_id=$2 AND n.entry_id=$1 AND n.node_id=$4 AND n.node_type='sense') AND EXISTS(SELECT 1 FROM lexicon.entry_publication_nodes n WHERE n.publication_id=$2 AND n.entry_id=$1 AND n.node_id=$5 AND n.node_type='form_slot') AND EXISTS(SELECT 1 FROM lexicon.entry_publication_nodes n WHERE n.publication_id=$2 AND n.entry_id=$1 AND n.node_id=$6 AND n.node_type='form_slot') AND EXISTS(SELECT 1 FROM lexicon.entry_publication_nodes n WHERE n.publication_id=$2 AND n.entry_id=$1 AND n.node_id=$7 AND n.node_type='form_variant')")
            .bind(entry)
            .bind(publication)
            .bind(pos)
            .bind(sense)
            .bind(base_form)
            .bind(form)
            .bind(variant)
            .fetch_one(&mut *conn)
            .await
            .map_err(AppError::internal)?
    } else if next {
        if let (Some(pos), Some(sense), Some(base_form), Some(form), Some(variant)) =
            (pos, sense, base_form, form, variant)
        {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon_next.entries WHERE id=$1 AND kind='word') AND EXISTS(SELECT 1 FROM lexicon_next.nodes WHERE id=$2 AND entry_id=$1 AND kind='sense') AND EXISTS(SELECT 1 FROM lexicon_next.form_editor_nodes WHERE id=$3 AND entry_id=$1 AND role='pos') AND EXISTS(SELECT 1 FROM lexicon_next.form_editor_nodes WHERE id=$4 AND entry_id=$1 AND role='form' AND parent_id=$3) AND EXISTS(SELECT 1 FROM lexicon_next.form_editor_nodes WHERE id=$5 AND entry_id=$1 AND role='form' AND parent_id=$3) AND EXISTS(SELECT 1 FROM lexicon_next.form_editor_nodes WHERE id=$6 AND entry_id=$1 AND role='variant' AND parent_id=$5)")
                .bind(entry).bind(sense).bind(pos).bind(base_form).bind(form).bind(variant)
                .fetch_one(&mut *conn).await.map_err(AppError::internal)?
        } else {
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM lexicon_next.entries WHERE id=$1 AND kind='word')",
            )
            .bind(entry)
            .fetch_one(&mut *conn)
            .await
            .map_err(AppError::internal)?
        }
    } else if let (Some(pos), Some(sense), Some(base_form), Some(form), Some(variant)) =
        (pos, sense, base_form, form, variant)
    {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM lexicon.entries e WHERE e.id=$1 AND e.kind='word') AND EXISTS(SELECT 1 FROM lexicon.nodes WHERE id=$2 AND entry_id=$1 AND node_type='pos') AND EXISTS(SELECT 1 FROM lexicon.senses sense WHERE sense.id=$3 AND sense.entry_id=$1 AND sense.entry_pos_id=$2) AND EXISTS(SELECT 1 FROM lexicon.v3_concrete_forms form WHERE form.id=$4 AND form.entry_id=$1 AND form.entry_pos_id=$2) AND EXISTS(SELECT 1 FROM lexicon.v3_concrete_forms base_form WHERE base_form.id=$5 AND base_form.entry_id=$1 AND base_form.entry_pos_id=$2) AND EXISTS(SELECT 1 FROM lexicon.v3_form_variants variant WHERE variant.id=$6 AND variant.entry_id=$1 AND variant.form_id=$4) AND ($4=$5 OR EXISTS(SELECT 1 FROM lexicon.v3_group_memberships form_group JOIN lexicon.v3_group_memberships base_group ON base_group.entry_id=form_group.entry_id AND base_group.form_group_id=form_group.form_group_id WHERE form_group.entry_id=$1 AND form_group.form_id=$4 AND base_group.form_id=$5))",
        )
            .bind(entry)
            .bind(pos)
            .bind(sense)
            .bind(form)
            .bind(base_form)
            .bind(variant)
            .fetch_one(&mut *conn).await.map_err(AppError::internal)?
    } else {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM lexicon.entries WHERE id=$1 AND kind='word')",
        )
        .bind(entry)
        .fetch_one(&mut *conn)
        .await
        .map_err(AppError::internal)?
    };
    if valid {
        Ok(())
    } else {
        Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "例句关联目标词条或词义不存在，请重新选择",
        ))
    }
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
    let mut sentence = decode(row)?;
    let mut conn = pool.acquire().await.map_err(AppError::internal)?;
    hydrate(&mut conn, &mut sentence).await?;
    Ok(sentence)
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
        // Associations belong to the variant and must be persisted even when
        // the sentence has no translations in the current medium.
        if let Some(associations) = &variant.associations {
            if !valid_source_segments(variant.content.text(), associations) {
                return Err(language::invalid());
            }
            sqlx::query(
                "DELETE FROM lexicon_next.sentence_associations WHERE source_variant_id=$1",
            )
            .bind(variant.id)
            .execute(&mut *conn)
            .await
            .map_err(AppError::internal)?;
            for association in associations {
                if association.source_segments.is_empty()
                    || association.source_dialect
                        != match variant.locale.as_str() {
                            "en-GB" => "uk",
                            "en-US" => "us",
                            _ => "common",
                        }
                {
                    return Err(language::invalid());
                }
                let first = association.source_segments.first().unwrap();
                let (
                    state,
                    target_kind,
                    target_source,
                    target_word_id,
                    target_publication_id,
                    target_pos_id,
                    target_base_form_id,
                    target_sense_id,
                    target_form_id,
                    target_variant_id,
                    target_dialect,
                    pending_headword,
                    pending_gloss,
                ) = match &association.target {
                    NextSentenceTarget::Linked {
                        target_source,
                        target_entry_id,
                        target_pos_id,
                        target_base_form_id,
                        target_form_id,
                        target_variant_id,
                        target_sense_id,
                        target_dialect,
                        target_publication_id,
                    } => (
                        "linked",
                        Some("word"),
                        Some(
                            target_source
                                .as_ref()
                                .map(|source| match source {
                                    crate::lexicon::dto::AssociationTargetSource::V3 => "v3",
                                    crate::lexicon::dto::AssociationTargetSource::LexiconNext => {
                                        "lexicon_next"
                                    }
                                })
                                .unwrap_or("v3"),
                        ),
                        Some(*target_entry_id),
                        *target_publication_id,
                        Some(*target_pos_id),
                        Some(*target_base_form_id),
                        Some(*target_sense_id),
                        Some(*target_form_id),
                        Some(*target_variant_id),
                        target_dialect.map(|v| v.as_str().to_string()),
                        None,
                        None,
                    ),
                    NextSentenceTarget::EntryOnly {
                        target_source,
                        target_entry_id,
                    } => (
                        "entry_only",
                        Some("word"),
                        Some(
                            target_source
                                .as_ref()
                                .map(|source| match source {
                                    crate::lexicon::dto::AssociationTargetSource::V3 => "v3",
                                    crate::lexicon::dto::AssociationTargetSource::LexiconNext => {
                                        "lexicon_next"
                                    }
                                })
                                .unwrap_or("v3"),
                        ),
                        Some(*target_entry_id),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    ),
                    NextSentenceTarget::Pending {
                        kind,
                        headword,
                        gloss,
                    } => (
                        "pending",
                        Some(kind.as_str()),
                        Some("v3"),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(headword.as_str()),
                        gloss.as_deref(),
                    ),
                };
                validate_target(conn, &association.target).await?;
                sqlx::query(
                    "INSERT INTO lexicon_next.sentence_associations (id,sentence_id,source_variant_id,source_dialect,source_segments,source_start,source_end,source_surface,state,target_kind,target_source,target_word_id,target_publication_id,target_pos_id,target_base_form_id,target_sense_id,target_form_id,target_variant_id,target_dialect,pending_headword,pending_gloss) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21)",
                )
                .bind(association.id)
                .bind(id)
                .bind(variant.id)
                .bind(&association.source_dialect)
                .bind(serde_json::to_value(&association.source_segments).map_err(AppError::internal)?)
                .bind(first.start as i32)
                .bind(association.source_segments.last().unwrap().end as i32)
                .bind(&first.surface)
                .bind(state)
                .bind(target_kind)
                .bind(target_source)
                .bind(target_word_id)
                .bind(target_publication_id)
                .bind(target_pos_id)
                .bind(target_base_form_id)
                .bind(target_sense_id)
                .bind(target_form_id)
                .bind(target_variant_id)
                .bind(target_dialect)
                .bind(pending_headword)
                .bind(pending_gloss)
                .execute(&mut *conn)
                .await
                .map_err(AppError::internal)?;
            }
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
