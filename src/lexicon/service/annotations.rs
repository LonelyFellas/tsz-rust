use std::collections::{BTreeMap, BTreeSet};

use sqlx::{Postgres, Transaction};

use super::*;
use crate::lexicon::dto::{
    EntryAnnotationConflict, EntryAnnotationConflictReason as Reason, EntryAnnotationGroup,
    EntryAnnotationResponse, EntryAnnotationUpdate, UpdateEntryAnnotationInput,
};
use crate::lexicon::repository::SurfaceLockKey;

pub(super) fn normalize_annotation(value: &mut Option<String>) -> Result<(), LexiconServiceError> {
    if let Some(text) = value {
        let normalized = text.trim();
        if normalized.chars().count() > 20 || normalized.chars().any(char::is_control) {
            return Err(LexiconServiceError::InvalidField {
                field: "annotation",
                message: "annotation must contain at most 20 characters and no control characters",
            });
        }
        *value = (!normalized.is_empty()).then(|| normalized.to_owned());
    }
    Ok(())
}

pub(super) fn normalize_updates(
    updates: &mut [EntryAnnotationUpdate],
) -> Result<(), LexiconServiceError> {
    let mut ids = BTreeSet::new();
    for update in updates.iter_mut() {
        normalize_annotation(&mut update.annotation)?;
        if update.base_annotation_revision < 1 || !ids.insert(update.entry_id) {
            return Err(LexiconServiceError::InvalidField {
                field: "annotation_updates",
                message: "entry ids must be unique and revisions positive",
            });
        }
    }
    updates.sort_by_key(|update| update.entry_id);
    Ok(())
}

#[derive(sqlx::FromRow)]
struct BaseMatch {
    entry_id: Uuid,
    dialect_scope: String,
    normalized_surface: String,
}

// Shared by creates and annotation edits. Existing surface writers retain their
// surface-key/context locks; this lock only serializes annotation group commands.
pub(super) async fn lock_annotation_commands(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), LexiconServiceError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('lexicon.annotation-command', 0))")
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    Ok(())
}

async fn base_keys(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Vec<String>, LexiconServiceError> {
    sqlx::query_scalar(
        r#"SELECT DISTINCT source.dialect_scope || ':' || source.normalized_surface
        FROM lexicon.surface_sources source
        JOIN lexicon.entries entry ON entry.id = source.entry_id
        WHERE source.entry_id = $1 AND source.is_deleted = FALSE
          AND (source.content_scope = 'draft' OR (source.content_scope = 'current_publication' AND source.publication_id = entry.current_publication_id))
          AND ((source.content_schema_version = 3 AND source.form_type = 'base')
            OR (source.content_schema_version = 2 AND source.source_kind = 'headword'))
        ORDER BY 1"#,
    ).bind(id).fetch_all(&mut **tx).await.map_err(database_error)
}

async fn lock_keys(
    tx: &mut Transaction<'_, Postgres>,
    keys: &[String],
) -> Result<(), LexiconServiceError> {
    LexiconRepository::lock_surface_policy_writer(tx)
        .await
        .map_err(repository_error)?;
    let keys = keys
        .iter()
        .map(|key| {
            let (dialect, surface) = key.split_once(':').ok_or_else(invariant_record)?;
            Ok(SurfaceLockKey {
                language: "en".to_owned(),
                dialect_scope: dialect.to_owned(),
                normalized_surface: surface.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, LexiconServiceError>>()?;
    LexiconRepository::lock_surface_keys(tx, &keys)
        .await
        .map_err(repository_error)
}

impl LexiconService {
    pub(super) async fn annotation_visible_entry_ids(
        &self,
        actor_id: Uuid,
        entry_ids: &[Uuid],
    ) -> Result<BTreeSet<Uuid>, LexiconServiceError> {
        if entry_ids.is_empty() {
            return Ok(BTreeSet::new());
        }
        // Same effective prototype/visibility rules as annotation_groups_in.
        // Inline the CTE so the requested IDs restrict the target-side source scan.
        let ids = sqlx::query_scalar::<_, Uuid>(r#"
            WITH visible_bases AS NOT MATERIALIZED (
                SELECT source.entry_id, entry.kind, source.language,
                       source.dialect_scope, source.normalized_surface
                FROM lexicon.surface_sources source
                JOIN lexicon.entries entry ON entry.id = source.entry_id
                WHERE source.language = 'en' AND source.is_deleted = FALSE
                  AND entry.archived_at IS NULL
                  AND ((source.content_schema_version = 3 AND source.source_kind = 'form_variant' AND source.form_type = 'base')
                    OR (source.content_schema_version = 2 AND source.source_kind = 'headword'))
                  AND ((source.content_scope = 'draft' AND entry.created_by_admin_id = $2)
                    OR (source.content_scope = 'current_publication' AND source.publication_id = entry.current_publication_id))
            )
            SELECT DISTINCT target.entry_id
            FROM visible_bases target
            JOIN visible_bases peer ON peer.entry_id <> target.entry_id
              AND peer.language = target.language AND peer.kind = target.kind
              AND peer.dialect_scope = target.dialect_scope
              AND peer.normalized_surface = target.normalized_surface
            WHERE target.entry_id = ANY($1)
        "#).bind(entry_ids).bind(actor_id).fetch_all(self.repository.pool()).await.map_err(database_error)?;
        Ok(ids.into_iter().collect())
    }

    async fn annotation_groups_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        kind: &str,
        keys: &[String],
    ) -> Result<Vec<EntryAnnotationGroup>, LexiconServiceError> {
        let mut dialects = Vec::new();
        let mut surfaces = Vec::new();
        for key in keys {
            let (dialect, surface) = key.split_once(':').ok_or_else(invariant_record)?;
            dialects.push(dialect);
            surfaces.push(surface);
        }
        let rows = sqlx::query_as::<_, BaseMatch>(r#"
            WITH requested AS (
                SELECT DISTINCT dialect_scope, normalized_surface
                FROM unnest($1::text[], $2::text[]) AS value(dialect_scope, normalized_surface)
            )
            SELECT DISTINCT source.entry_id, source.dialect_scope, source.normalized_surface
            FROM requested
            JOIN lexicon.surface_sources source ON source.language = 'en'
              AND source.dialect_scope = requested.dialect_scope
              AND source.normalized_surface = requested.normalized_surface
              AND source.is_deleted = FALSE
            JOIN lexicon.entries entry ON entry.id = source.entry_id
            WHERE entry.archived_at IS NULL AND entry.kind = $4
              AND ((source.content_schema_version = 3 AND source.source_kind = 'form_variant' AND source.form_type = 'base')
                OR (source.content_schema_version = 2 AND source.source_kind = 'headword'))
              AND ((source.content_scope = 'draft' AND entry.created_by_admin_id = $3)
                OR (source.content_scope = 'current_publication' AND source.publication_id = entry.current_publication_id))
            ORDER BY source.dialect_scope, source.normalized_surface, source.entry_id
        "#).bind(dialects).bind(surfaces).bind(actor_id).bind(kind)
          .fetch_all(&mut **tx).await.map_err(database_error)?;
        let mut groups: BTreeMap<(String, String), Vec<Uuid>> = BTreeMap::new();
        for row in rows {
            groups
                .entry((row.dialect_scope, row.normalized_surface))
                .or_default()
                .push(row.entry_id);
        }
        Ok(groups
            .into_iter()
            .map(
                |((dialect_scope, normalized_surface), entry_ids)| EntryAnnotationGroup {
                    dialect_scope,
                    normalized_surface,
                    entry_ids,
                },
            )
            .collect())
    }

    async fn annotation_conflict_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        groups: Vec<EntryAnnotationGroup>,
        extra: Option<Uuid>,
    ) -> Result<EntryAnnotationConflict, LexiconServiceError> {
        let ids = groups
            .iter()
            .flat_map(|group| group.entry_ids.iter().copied())
            .chain(extra)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        LexiconRepository::lock_surface_contexts(tx, &ids)
            .await
            .map_err(repository_error)?;
        let entries = self.v3_surface_contexts_in(tx, &ids, actor_id).await?;
        Ok(EntryAnnotationConflict {
            reason: Reason::Required,
            entries,
            groups,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn apply_create_annotations(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        request_id: Uuid,
        kind: &str,
        keys: &[String],
        annotation: &Option<String>,
        updates: &[EntryAnnotationUpdate],
    ) -> Result<(), LexiconServiceError> {
        let groups = self.annotation_groups_in(tx, actor_id, kind, keys).await?;
        let mut conflict = self
            .annotation_conflict_in(tx, actor_id, groups, None)
            .await?;
        let submitted = updates
            .iter()
            .map(|update| (update.entry_id, update))
            .collect::<BTreeMap<_, _>>();
        let current = conflict
            .entries
            .iter()
            .map(|entry| entry.entry_id)
            .collect::<BTreeSet<_>>();
        let fail = if submitted.keys().any(|id| !current.contains(id)) {
            Some(Reason::GroupChanged)
        } else if !current.is_empty()
            && (annotation.is_none()
                || submitted.len() != current.len()
                || updates.iter().any(|update| update.annotation.is_none()))
        {
            Some(Reason::Required)
        } else if conflict.entries.iter().any(|entry| {
            submitted
                .get(&entry.entry_id)
                .is_some_and(|update| update.base_annotation_revision != entry.annotation_revision)
        }) {
            Some(Reason::RevisionConflict)
        } else {
            let duplicate = conflict.groups.iter().any(|group| {
                let mut labels = BTreeSet::new();
                if let Some(value) = annotation {
                    labels.insert(value.to_lowercase());
                }
                group.entry_ids.iter().any(|id| {
                    submitted
                        .get(id)
                        .and_then(|update| update.annotation.as_ref())
                        .is_some_and(|value| !labels.insert(value.to_lowercase()))
                })
            });
            duplicate.then_some(Reason::Duplicate)
        };
        if let Some(reason) = fail {
            conflict.reason = reason;
            return Err(LexiconServiceError::AnnotationConflict(Box::new(conflict)));
        }
        // Updating an old entry also affects its prototypes outside the new
        // entry's groups. Validate those edges without broadening the create UI.
        for update in updates {
            let old = conflict
                .entries
                .iter()
                .find(|entry| entry.entry_id == update.entry_id)
                .ok_or_else(invariant_record)?;
            if old.annotation == update.annotation {
                continue;
            }
            let old_keys = base_keys(tx, update.entry_id).await?;
            lock_keys(tx, &old_keys).await?;
            let other_groups = self
                .annotation_groups_in(tx, actor_id, kind, &old_keys)
                .await?;
            let other = self
                .annotation_conflict_in(tx, actor_id, other_groups, None)
                .await?;
            if base_keys(tx, update.entry_id).await? != old_keys {
                return Err(LexiconServiceError::ReferenceConflict);
            }
            let duplicate = other
                .entries
                .iter()
                .filter(|entry| entry.entry_id != update.entry_id)
                .any(|entry| {
                    let effective = submitted
                        .get(&entry.entry_id)
                        .map_or(&entry.annotation, |value| &value.annotation);
                    effective
                        .as_ref()
                        .zip(update.annotation.as_ref())
                        .is_some_and(|(a, b)| a.to_lowercase() == b.to_lowercase())
                });
            if duplicate {
                conflict.reason = Reason::Duplicate;
                return Err(LexiconServiceError::AnnotationConflict(Box::new(conflict)));
            }
        }
        for update in updates {
            self.persist_annotation_in(
                tx,
                actor_id,
                request_id,
                update.entry_id,
                &update.annotation,
                update.base_annotation_revision,
            )
            .await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn persist_annotation_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        request_id: Uuid,
        id: Uuid,
        annotation: &Option<String>,
        base_revision: i64,
    ) -> Result<EntryAnnotationResponse, LexiconServiceError> {
        let row = sqlx::query_as::<_, (Option<String>, i64, i64)>(r#"
            UPDATE lexicon.entries SET annotation = $2,
                annotation_revision = annotation_revision + CASE WHEN annotation IS DISTINCT FROM $2 THEN 1 ELSE 0 END
            WHERE id = $1 AND annotation_revision = $3
            RETURNING annotation, annotation_revision, revision
        "#).bind(id).bind(annotation).bind(base_revision).fetch_optional(&mut **tx).await.map_err(database_error)?.ok_or(LexiconServiceError::ReferenceConflict)?;
        if row.1 != base_revision {
            sqlx::query(r#"INSERT INTO audit.admin_actions
                (id, actor_admin_id, action, resource_type, resource_id, resource_revision, request_id, metadata)
                VALUES ($1, $2, 'lexicon.entry.annotation_updated', 'lexicon.entry', $3, $4, $5, $6)"#)
                .bind(Uuid::now_v7()).bind(actor_id).bind(id).bind(row.2).bind(request_id)
                .bind(serde_json::json!({"annotation": annotation, "base_annotation_revision": base_revision, "annotation_revision": row.1}))
                .execute(&mut **tx).await.map_err(database_error)?;
        }
        Ok(EntryAnnotationResponse {
            entry_id: id,
            annotation: row.0,
            annotation_revision: row.1,
        })
    }

    pub async fn update_annotation(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        id: Uuid,
        mut input: UpdateEntryAnnotationInput,
    ) -> Result<EntryAnnotationResponse, LexiconServiceError> {
        normalize_annotation(&mut input.annotation)?;
        if input.base_annotation_revision < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_annotation_revision",
                message: "revision must be positive",
            });
        }
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        lock_annotation_commands(&mut tx).await?;
        let kind =
            sqlx::query_scalar::<_, String>("SELECT kind FROM lexicon.entries WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(database_error)?
                .ok_or(LexiconServiceError::WordNotFound)?;
        let keys = base_keys(&mut tx, id).await?;
        lock_keys(&mut tx, &keys).await?;
        let mut groups = self
            .annotation_groups_in(&mut tx, actor_id, &kind, &keys)
            .await?;
        // The target's draft is editable by active admins even when candidate
        // discovery would hide that draft from them. Do not expose other drafts.
        for group in &mut groups {
            if !group.entry_ids.contains(&id) {
                group.entry_ids.push(id);
                group.entry_ids.sort();
            }
        }
        groups.retain(|group| group.entry_ids.len() > 1);
        let mut conflict = self
            .annotation_conflict_in(&mut tx, actor_id, groups, Some(id))
            .await?;
        if base_keys(&mut tx, id).await? != keys {
            return Err(LexiconServiceError::ReferenceConflict);
        }
        let archived = sqlx::query_scalar::<_, bool>(
            "SELECT archived_at IS NOT NULL FROM lexicon.entries WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        if archived {
            return Err(LexiconServiceError::EntryArchived);
        }
        let target = conflict
            .entries
            .iter()
            .find(|entry| entry.entry_id == id)
            .ok_or(LexiconServiceError::WordNotFound)?;
        let reason = if target.annotation_revision != input.base_annotation_revision {
            Some(Reason::RevisionConflict)
        } else if !conflict.groups.is_empty() && input.annotation.is_none() {
            Some(Reason::Required)
        } else if conflict
            .entries
            .iter()
            .filter(|entry| entry.entry_id != id)
            .any(|entry| {
                entry
                    .annotation
                    .as_ref()
                    .zip(input.annotation.as_ref())
                    .is_some_and(|(a, b)| a.to_lowercase() == b.to_lowercase())
            })
        {
            Some(Reason::Duplicate)
        } else {
            None
        };
        if let Some(reason) = reason {
            conflict.reason = reason;
            return Err(LexiconServiceError::AnnotationConflict(Box::new(conflict)));
        }
        let response = self
            .persist_annotation_in(
                &mut tx,
                actor_id,
                request_id,
                id,
                &input.annotation,
                input.base_annotation_revision,
            )
            .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(response)
    }
}
