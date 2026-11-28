//! Sentence edits share the word-draft transaction; no partial resource writes.
use super::{
    model::{NextDraft, NextEntry},
    sentence_details,
    sentence_model::*,
    sentences,
};
use crate::{
    admin::{AdminAuth, permissions},
    error::{AppError, ErrorCode},
};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn invalid(message: &str) -> AppError {
    AppError::unprocessable(ErrorCode::InvalidRequestBody, message)
}

pub(super) fn has_associations(changes: &[NextSentenceDraftChange]) -> bool {
    changes
        .iter()
        .any(|change| change.variant.associations.as_ref().is_some())
}
pub(super) async fn resources(
    conn: &mut PgConnection,
    draft: &NextDraft,
) -> Result<Vec<NextSentence>, AppError> {
    let ids: BTreeSet<_> = draft
        .senses
        .iter()
        .flat_map(|sense| sense.sentence_ids.iter().flatten().copied())
        .collect();
    if ids.len() > 1000 {
        return Err(invalid("例句引用数量超出限制"));
    }
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<_> = ids.into_iter().collect();
    let rows = sqlx::query("SELECT s.id,s.target_language,s.owner_id,s.revision,s.updated_at,s.document,(SELECT count(*) FROM lexicon_next.sentence_references r WHERE r.sentence_id=s.id) AS reference_count FROM lexicon_next.sentences s WHERE s.id=ANY($1) ORDER BY s.id")
        .bind(&ids).fetch_all(&mut *conn).await.map_err(AppError::internal)?;
    if rows.len() != ids.len() {
        return Err(invalid("部分关联例句不存在，请重新加载词条"));
    }
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let mut sentence = sentences::decode(row)?;
        sentences::hydrate(conn, &mut sentence).await?;
        result.push(sentence);
    }
    Ok(result)
}

pub(super) async fn save_changes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    auth: &AdminAuth,
    entry: &NextEntry,
    draft: &NextDraft,
    changes: Vec<NextSentenceDraftChange>,
    run: Option<&str>,
) -> Result<(), AppError> {
    if changes.is_empty() {
        return Ok(());
    }
    if changes.len() > 1000 {
        return Err(invalid("例句修改数量超出限制"));
    }
    let access = permissions::lock(tx, auth.subject).await?;
    let conn = &mut **tx;
    access.require("sentences.access")?;
    let refs: BTreeSet<_> = draft
        .senses
        .iter()
        .flat_map(|sense| sense.sentence_ids.iter().flatten().copied())
        .collect();
    let mut groups = BTreeMap::<Uuid, Vec<NextSentenceDraftChange>>::new();
    for mut change in changes {
        if change.expected_revision.is_some_and(|v| v < 1)
            || change.id.is_nil()
            || !refs.contains(&change.id)
            || entry.medium_locale.as_deref() != Some(change.medium_locale.as_str())
        {
            return Err(invalid("例句修改必须属于当前词条引用及媒介语言"));
        }
        sentences::scoped_variant(
            &change.target_locale,
            &change.medium_locale,
            &change.variant,
        )?;
        sentence_details::normalize(&mut change.variant)?;
        groups.entry(change.id).or_default().push(change);
    }
    for (id, changes) in groups {
        let expected = changes[0].expected_revision;
        let mut scopes = BTreeSet::new();
        if changes
            .iter()
            .any(|c| c.expected_revision != expected || !scopes.insert(c.target_locale.clone()))
        {
            return Err(invalid("同一例句的版本必须一致，语言修改不能重复"));
        }
        let existing: Option<(Uuid, String, i64, serde_json::Value)> = sqlx::query_as("SELECT owner_id,target_language,revision,document FROM lexicon_next.sentences WHERE id=$1 FOR UPDATE")
            .bind(id).fetch_optional(&mut *conn).await.map_err(AppError::internal)?;
        let mut document;
        if let Some((owner, language, revision, value)) = existing {
            access.require_owned_action("sentences.edit", owner)?;
            let existing_run = crate::test_resources::sentence_run(conn, id).await?;
            crate::test_resources::require_same_run(run, existing_run.as_deref())?;
            if language != entry.target_language || expected != Some(revision) {
                return Err(AppError::conflict(
                    ErrorCode::RevisionConflict,
                    Some("sentence_changes"),
                    "例句已更新，所有修改均未保存；草稿已保留，请重新加载比较",
                ));
            }
            document =
                serde_json::from_value::<NextSentenceDraft>(value).map_err(AppError::internal)?;
        } else {
            access.require("sentences.create")?;
            if expected.is_some() {
                return Err(AppError::conflict(
                    ErrorCode::RevisionConflict,
                    Some("sentence_changes"),
                    "例句不存在，所有修改均未保存；草稿已保留",
                ));
            }
            let foreign: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon.shared_sentences WHERE id=$1 UNION ALL SELECT 1 FROM lexicon_next.entries WHERE id=$1)")
                .bind(id).fetch_one(&mut *conn).await.map_err(AppError::internal)?;
            if foreign {
                return Err(invalid("例句身份已属于其他资源"));
            }
            let digest =
                Sha256::digest(serde_json::to_vec(&changes).map_err(AppError::internal)?).to_vec();
            document = NextSentenceDraft {
                schema_version: 1,
                variants: vec![],
            };
            // A racing creator is a conflict, not a second resource or an overwrite.
            let inserted = sqlx::query("INSERT INTO lexicon_next.sentences(id,target_language,owner_id,revision,document,create_digest) VALUES($1,$2,$3,1,$4,$5) ON CONFLICT(id) DO NOTHING")
                .bind(id).bind(&entry.target_language).bind(auth.subject).bind(serde_json::to_value(&document).map_err(AppError::internal)?).bind(digest)
                .execute(&mut *conn).await.map_err(AppError::internal)?.rows_affected();
            if inserted == 0 {
                return Err(AppError::conflict(
                    ErrorCode::RevisionConflict,
                    Some("sentence_changes"),
                    "例句身份已使用，草稿已保留，请重新加载比较",
                ));
            }
            if let Some(run) = run {
                crate::test_resources::register_sentence(conn, run, id).await?;
            }
        }
        let changed_locales: Vec<_> = changes.iter().map(|c| c.target_locale.clone()).collect();
        for mut change in changes {
            if let Some(index) = document
                .variants
                .iter()
                .position(|v| v.locale == change.target_locale)
            {
                sentences::merge_variant(
                    &document.variants[index],
                    &mut change.variant,
                    &change.medium_locale,
                )?;
                document.variants[index] = change.variant;
            } else {
                document.variants.push(change.variant);
            }
        }
        sentences::validate(id, &document)?;
        for locale in changed_locales {
            sentences::prepare(conn, auth.subject, id, &mut document, &locale).await?;
        }
        sqlx::query("UPDATE lexicon_next.sentences SET document=$2,revision=revision+$3,updated_at=clock_timestamp() WHERE id=$1")
            .bind(id).bind(serde_json::to_value(document).map_err(AppError::internal)?).bind(if expected.is_some() {1i64} else {0})
            .execute(&mut *conn).await.map_err(AppError::internal)?;
    }
    Ok(())
}
