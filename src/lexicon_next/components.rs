use std::collections::{HashMap, HashSet};

use serde_json::Value;
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

use super::{
    language,
    model::{NextDraft, NextEntry, NextEntryKind, NextPhraseComponentUsage, NextSense},
    publication_model::NextPublicationIssue,
};
use crate::{
    error::{AppError, ErrorCode},
    lexicon::{
        dto::{AssociationTargetSource, PhraseComponentUsageV3},
        normalization::normalize_headword,
    },
};

/// Called under the target catalog's lifecycle lock. For a batch, ignore
/// source entries whose relationships are being archived or removed with it.
pub(crate) async fn ensure_target_not_referenced(
    conn: &mut PgConnection,
    target_source: AssociationTargetSource,
    targets: &[Uuid],
    excluded_sources: &[Uuid],
    include_archived: bool,
) -> Result<(), AppError> {
    let source = match target_source {
        AssociationTargetSource::V3 => "v3",
        AssociationTargetSource::LexiconNext => "lexicon_next",
    };
    let referenced: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
            SELECT FROM lexicon_next.entries e
            CROSS JOIN LATERAL (
                SELECT d.document FROM lexicon_next.draft_revisions d
                WHERE d.entry_id=e.id AND d.revision=e.revision
                UNION ALL
                (SELECT p.snapshot->'draft' FROM lexicon_next.publications p
                 WHERE p.entry_id=e.id AND (e.status='published' OR $4)
                 ORDER BY p.lifecycle_revision DESC LIMIT 1)
            ) documents
            WHERE e.kind='phrase' AND ($4 OR e.status<>'archived')
              AND e.id <> ALL($3)
              AND jsonb_path_exists(
                  documents.document,
                  '$.senses[*].component_usages[*] ? (@.target_source == $source && @.usage.state == "resolved" && @.usage.target_word_id == $ids[*])',
                  jsonb_build_object('source',$1::text,'ids',to_jsonb($2::uuid[]))
              )
        )"#,
    )
    .bind(source)
    .bind(targets)
    .bind(excluded_sources)
    .bind(include_archived)
    .fetch_one(conn)
    .await
    .map_err(AppError::internal)?;
    if referenced {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "词条仍被短语成分用词引用，请先更新引用方",
        ));
    }
    Ok(())
}

/// Check the final batch state when a published phrase becomes active again.
pub(super) async fn ensure_restorable(
    conn: &mut PgConnection,
    restored: &[Uuid],
) -> Result<(), AppError> {
    let unavailable: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
            SELECT FROM lexicon_next.entries e
            CROSS JOIN LATERAL (
                SELECT snapshot FROM lexicon_next.publications
                WHERE entry_id=e.id ORDER BY lifecycle_revision DESC LIMIT 1
            ) p
            CROSS JOIN LATERAL jsonb_path_query(
                p.snapshot->'draft', '$.senses[*].component_usages[*] ? (@.usage.state == "resolved")'
            ) ref
            WHERE e.id=ANY($1) AND e.kind='phrase' AND e.status='published' AND (
                (ref->>'target_source'='lexicon_next' AND NOT EXISTS (
                    SELECT FROM lexicon_next.entries target
                    JOIN lexicon_next.publications published ON published.entry_id=target.id
                    WHERE target.id=(ref->'usage'->>'target_word_id')::uuid
                      AND published.id=(ref->'usage'->>'target_publication_id')::uuid
                      AND target.status='published'
                )) OR
                (ref->>'target_source'='v3' AND NOT EXISTS (
                    SELECT FROM lexicon.entries target
                    JOIN lexicon.entry_publications published ON published.entry_id=target.id
                    WHERE target.id=(ref->'usage'->>'target_word_id')::uuid
                      AND published.id=(ref->'usage'->>'target_publication_id')::uuid
                      AND target.archived_at IS NULL
                    FOR SHARE OF target NOWAIT
                ))
            )
        )"#,
    )
    .bind(restored)
    .fetch_one(conn)
    .await
    .map_err(|error| {
        if error.as_database_error().and_then(|error| error.code()).as_deref() == Some("55P03") {
            return AppError::conflict(
                ErrorCode::RevisionConflict,
                None,
                "成分用词的目标正在变更，请重试",
            );
        }
        AppError::internal(error)
    })?;
    if unavailable {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "已发布成分用词的目标尚未恢复，请先恢复目标",
        ));
    }
    Ok(())
}

pub(super) fn has_usages(draft: &NextDraft) -> bool {
    draft
        .senses
        .iter()
        .any(|sense| sense.component_usages.is_some())
}

pub(super) fn strip(draft: &mut NextDraft) {
    for sense in &mut draft.senses {
        sense.component_usages = None;
    }
}

pub(super) fn validate(sense: &NextSense, ids: &mut HashSet<Uuid>) -> Result<(), AppError> {
    let mut literals = HashSet::new();
    let rows = sense.component_usages.as_deref().unwrap_or_default();
    if rows.len() > 100 {
        return Err(language::invalid());
    }
    for row in rows {
        let PhraseComponentUsageV3::Resolved { id, literal, .. } = &row.usage else {
            return Err(language::invalid());
        };
        language::identity(ids, *id)?;
        language::text(literal, 200)?;
        if literal.trim() != literal || !literals.insert(literal) {
            return Err(language::invalid());
        }
    }
    Ok(())
}

pub(super) async fn prepare(
    tx: &mut Transaction<'_, Postgres>,
    draft: &mut NextDraft,
    previous: &NextEntry,
) -> Result<(), AppError> {
    if has_usages(draft) && previous.kind != NextEntryKind::Phrase {
        return Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "只有短语词条可以设置成分用词",
        ));
    }
    let old: HashMap<_, _> = previous
        .draft
        .senses
        .iter()
        .map(|sense| (sense.id, &sense.component_usages))
        .collect();
    for sense in &mut draft.senses {
        let saved = old.get(&sense.id).and_then(|rows| rows.as_deref());
        let Some(rows) = &mut sense.component_usages else {
            sense.component_usages = saved.map(<[_]>::to_vec);
            continue;
        };
        for row in rows {
            // Unrelated edits remain available if a saved target has since changed.
            if saved.is_some_and(|saved| {
                saved.iter().any(|existing| {
                    existing.target_source == row.target_source && existing.usage == row.usage
                })
            }) {
                continue;
            }
            if !resolve(tx, previous.id, previous.medium_locale.as_deref(), row).await? {
                return Err(AppError::unprocessable(
                    ErrorCode::InvalidRequestBody,
                    "成分用词关联的词形或词义已不可用，请重新选择",
                ));
            }
        }
    }
    Ok(())
}

pub(super) async fn publication_issues(
    tx: &mut Transaction<'_, Postgres>,
    entry: &mut NextEntry,
) -> Result<Vec<NextPublicationIssue>, AppError> {
    let mut issues = Vec::new();
    for sense in &mut entry.draft.senses {
        for row in sense.component_usages.iter_mut().flatten() {
            let available = resolve(tx, entry.id, entry.medium_locale.as_deref(), row).await?;
            let message = if !available {
                "成分用词关联的词形或词义已不可用，请重新选择"
            } else if matches!(
                row.usage,
                PhraseComponentUsageV3::Resolved {
                    target_publication_id: None,
                    ..
                }
            ) {
                "成分用词关联的词形或词义尚未发布，请先发布目标"
            } else {
                continue;
            };
            issues.push(NextPublicationIssue {
                node_id: sense.id,
                field: "component_usages".into(),
                message: message.into(),
            });
        }
    }
    Ok(issues)
}

async fn resolve(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    medium: Option<&str>,
    row: &mut NextPhraseComponentUsage,
) -> Result<bool, AppError> {
    let PhraseComponentUsageV3::Resolved {
        target_word_id,
        target_publication_id,
        ..
    } = &row.usage
    else {
        return Ok(false);
    };
    if *target_word_id == entry_id {
        return Ok(false);
    }
    if matches!(row.target_source, AssociationTargetSource::V3) {
        return crate::lexicon::service::v3::prepare_external_phrase_component(tx, &mut row.usage)
            .await
            .map_err(crate::lexicon::handler::map_error);
    }

    let pinned = *target_publication_id;
    let target = sqlx::query_as::<_, (Value, Option<Uuid>, Option<Value>)>(
        "SELECT r.document,p.id,p.snapshot->'draft'
         FROM lexicon_next.entries e
         JOIN lexicon_next.draft_revisions r ON r.entry_id=e.id AND r.revision=e.revision
         LEFT JOIN LATERAL (
            SELECT id,snapshot FROM lexicon_next.publications
            WHERE entry_id=e.id AND ($3::uuid IS NULL OR id=$3)
            ORDER BY lifecycle_revision DESC LIMIT 1
         ) p ON e.status='published'
         WHERE e.id=$1 AND e.medium_locale=$2 AND e.kind='word' AND e.status<>'archived'",
    )
    .bind(target_word_id)
    .bind(medium.unwrap_or("zh-Hans"))
    .bind(pinned)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    let Some((draft, publication_id, publication)) = target else {
        return Ok(false);
    };
    let medium = medium.unwrap_or("zh-Hans");
    if let Some(published) = publication
        && apply_next_target(published, medium, publication_id, &mut row.usage)?
    {
        return Ok(true);
    }
    if pinned.is_some() {
        return Ok(false);
    }
    apply_next_target(draft, medium, None, &mut row.usage)
}

fn apply_next_target(
    mut document: Value,
    medium: &str,
    publication: Option<Uuid>,
    usage: &mut PhraseComponentUsageV3,
) -> Result<bool, AppError> {
    let PhraseComponentUsageV3::Resolved {
        literal,
        target_publication_id,
        target_pos_id,
        target_base_form_id,
        target_sense_id,
        target_form_id,
        target_variant_id,
        target_dialect,
        target_form_type,
        target_headword,
        target_gloss,
        ..
    } = usage
    else {
        return Ok(false);
    };
    super::model::strip_legacy_creation_detection(&mut document);
    let draft: NextDraft = serde_json::from_value(document).map_err(AppError::internal)?;
    let Some(editor) = draft.form_editor else {
        return Ok(false);
    };
    let forms = editor.resolve().map_err(|error| error.into_app_error())?;
    let Some(pos) = forms.pos.iter().find(|pos| pos.pos_id == *target_pos_id) else {
        return Ok(false);
    };
    let Some(form) = pos.forms.iter().find(|form| form.id == *target_form_id) else {
        return Ok(false);
    };
    let Some(sense) = draft.senses.iter().find(|sense| {
        sense.id == *target_sense_id && sense.part_of_speech.as_deref() == Some(pos.pos.as_str())
    }) else {
        return Ok(false);
    };
    let base_exists = pos
        .forms
        .iter()
        .any(|f| f.id == *target_base_form_id && f.form_type == "base");
    let in_same_group = target_base_form_id == target_form_id
        || pos.form_groups.iter().any(|group| {
            group
                .members
                .iter()
                .any(|member| member.form_id == *target_base_form_id)
                && group
                    .members
                    .iter()
                    .any(|member| member.form_id == *target_form_id)
        });
    let Some(variant) = form
        .regional_variants
        .variants()
        .into_iter()
        .find(|variant| variant.id == *target_variant_id && variant.dialect == *target_dialect)
    else {
        return Ok(false);
    };
    let matches_literal = normalize_headword(variant.spelling)
        .ok()
        .zip(normalize_headword(literal).ok())
        .is_some_and(|(form, token)| form.key == token.key);
    let Some(gloss) = sense
        .definitions
        .iter()
        .find(|def| def.locale == medium && !def.text.trim().is_empty())
        .map(|def| &def.text)
        .or_else(|| {
            sense
                .equivalents
                .iter()
                .find(|row| row.locale == medium && !row.text.trim().is_empty())
                .map(|row| &row.text)
        })
    else {
        return Ok(false);
    };
    if !base_exists || !in_same_group || !matches_literal {
        return Ok(false);
    }
    // A picker choice without a publication ID came from the current draft.
    // Only pin it to a publication that has the same displayed content.
    if publication.is_some()
        && target_publication_id.is_none()
        && (target_headword.as_str() != variant.spelling
            || target_form_type.as_str() != form.form_type
            || target_gloss.as_str() != gloss)
    {
        return Ok(false);
    }
    *target_publication_id = publication;
    target_form_type.clone_from(&form.form_type);
    target_headword.clone_from(variant.spelling);
    target_gloss.clone_from(gloss);
    Ok(true)
}
