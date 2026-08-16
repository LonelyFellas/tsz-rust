use std::collections::BTreeSet;

use super::*;
use crate::lexicon::{
    dto::{DraftFormsStepContent, WordFormVariantV2},
    surface::{NormalizedSurfaceScope, normalize_surface_scopes},
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SurfaceLockKey {
    pub language: String,
    pub dialect_scope: String,
    pub normalized_surface: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SurfaceProjectionSource {
    pub entry_id: Uuid,
    pub source_id: String,
    pub source_kind: &'static str,
    pub source_node_id: Option<Uuid>,
    pub language: String,
    pub entry_kind: &'static str,
    pub dialect: &'static str,
    pub dialect_scope: &'static str,
    pub surface: String,
    pub normalized_surface: String,
    pub normalization_version: i16,
    pub pos_id: Option<Uuid>,
    pub pos: Option<String>,
    pub form_type: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SurfaceContentScope {
    Draft,
    CurrentPublication(Uuid),
}

impl SurfaceContentScope {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::CurrentPublication(_) => "current_publication",
        }
    }

    const fn publication_id(self) -> Option<Uuid> {
        match self {
            Self::Draft => None,
            Self::CurrentPublication(id) => Some(id),
        }
    }
}

pub(crate) fn surface_projection_sources(
    word: &AdminWordV2,
) -> Result<Vec<SurfaceProjectionSource>, HeadwordNormalizationError> {
    let mut sources = Vec::new();
    match &word.headwords {
        WordHeadwordsV2::Unified { common } => push_source_scopes(
            &mut sources,
            word,
            format!("entry:{}:headword:common", word.id),
            "headword",
            None,
            common,
            Dialect::Common,
            None,
            None,
            None,
        )?,
        WordHeadwordsV2::Distinguish { uk, us, .. } => {
            push_source_scopes(
                &mut sources,
                word,
                format!("entry:{}:headword:uk", word.id),
                "headword",
                None,
                uk,
                Dialect::Uk,
                None,
                None,
                None,
            )?;
            push_source_scopes(
                &mut sources,
                word,
                format!("entry:{}:headword:us", word.id),
                "headword",
                None,
                us,
                Dialect::Us,
                None,
                None,
                None,
            )?;
        }
    }
    push_form_sources(&mut sources, word, &word.forms)?;
    Ok(sources)
}

fn push_form_sources(
    sources: &mut Vec<SurfaceProjectionSource>,
    word: &AdminWordV2,
    forms: &DraftFormsStepContent,
) -> Result<(), HeadwordNormalizationError> {
    for pos in &forms.pos {
        for variant in &pos.base_form.variants {
            push_form_variant(
                sources,
                word,
                pos.pos_id,
                &pos.pos,
                &pos.base_form.form_type,
                variant,
            )?;
        }
        for group in &pos.form_groups {
            for slot in &group.slots {
                for variant in &slot.variants {
                    push_form_variant(
                        sources,
                        word,
                        pos.pos_id,
                        &pos.pos,
                        &slot.form_type,
                        variant,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn push_form_variant(
    sources: &mut Vec<SurfaceProjectionSource>,
    word: &AdminWordV2,
    pos_id: Uuid,
    pos: &str,
    form_type: &str,
    variant: &WordFormVariantV2,
) -> Result<(), HeadwordNormalizationError> {
    push_source_scopes(
        sources,
        word,
        format!("entry:{}:form:{}", word.id, variant.id),
        "form",
        Some(variant.id),
        &variant.spelling,
        variant.dialect,
        Some(pos_id),
        Some(pos.to_owned()),
        Some(form_type.to_owned()),
    )
}

#[allow(clippy::too_many_arguments)]
fn push_source_scopes(
    sources: &mut Vec<SurfaceProjectionSource>,
    word: &AdminWordV2,
    source_id: String,
    source_kind: &'static str,
    source_node_id: Option<Uuid>,
    surface: &str,
    dialect: Dialect,
    pos_id: Option<Uuid>,
    pos: Option<String>,
    form_type: Option<String>,
) -> Result<(), HeadwordNormalizationError> {
    for scope in normalize_surface_scopes(surface, dialect)? {
        sources.push(projection_source(
            word,
            source_id.clone(),
            source_kind,
            source_node_id,
            scope,
            pos_id,
            pos.clone(),
            form_type.clone(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn projection_source(
    word: &AdminWordV2,
    source_id: String,
    source_kind: &'static str,
    source_node_id: Option<Uuid>,
    scope: NormalizedSurfaceScope,
    pos_id: Option<Uuid>,
    pos: Option<String>,
    form_type: Option<String>,
) -> SurfaceProjectionSource {
    SurfaceProjectionSource {
        entry_id: word.id,
        source_id,
        source_kind,
        source_node_id,
        language: word.language.clone(),
        entry_kind: kind_string(word.kind),
        dialect: scope.dialect,
        dialect_scope: scope.dialect_scope,
        surface: scope.surface,
        normalized_surface: scope.normalized_surface,
        normalization_version: scope.normalization_version,
        pos_id,
        pos,
        form_type,
    }
}

pub(crate) fn surface_lock_keys<'a>(
    source_sets: impl IntoIterator<Item = &'a [SurfaceProjectionSource]>,
) -> Vec<SurfaceLockKey> {
    source_sets
        .into_iter()
        .flat_map(|sources| sources.iter())
        .map(|source| SurfaceLockKey {
            language: source.language.clone(),
            dialect_scope: source.dialect_scope.to_owned(),
            normalized_surface: source.normalized_surface.clone(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

impl LexiconRepository {
    /// Joins the surface-policy writer barrier for the lifetime of `transaction`.
    ///
    /// This is public so database contract tests can prove that a policy disable
    /// waits for every in-flight create which entered under the previous epoch.
    pub async fn lock_surface_policy_writer(
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(), LexiconRepositoryError> {
        sqlx::query(
            "SELECT pg_advisory_xact_lock_shared(hashtextextended('lexicon.surface-policy-writer', 0))",
        )
        .execute(&mut **tx)
        .await
        .map(|_| ())
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn current_publication_surface_sources(
        tx: &mut Transaction<'_, Postgres>,
        entry_ids: &[Uuid],
    ) -> Result<Vec<SurfaceProjectionSource>, LexiconRepositoryError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        let snapshots = sqlx::query_scalar::<_, serde_json::Value>(
            r#"
            SELECT publication.snapshot
            FROM lexicon.entries entry
            JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
             AND publication.entry_id = entry.id
            WHERE entry.id = ANY($1)
            ORDER BY entry.id
            "#,
        )
        .bind(entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        let mut sources = Vec::new();
        for snapshot in snapshots {
            let word: AdminWordV2 = serde_json::from_value(snapshot)?;
            sources.extend(surface_projection_sources(&word).map_err(|_| {
                LexiconRepositoryError::Invariant(
                    "current publication surface normalization failed",
                )
            })?);
        }
        Ok(sources)
    }

    pub(crate) async fn lock_surface_keys(
        tx: &mut Transaction<'_, Postgres>,
        keys: &[SurfaceLockKey],
    ) -> Result<(), LexiconRepositoryError> {
        if keys.is_empty() {
            return Ok(());
        }
        let languages = keys
            .iter()
            .map(|key| key.language.as_str())
            .collect::<Vec<_>>();
        let dialect_scopes = keys
            .iter()
            .map(|key| key.dialect_scope.as_str())
            .collect::<Vec<_>>();
        let normalized_surfaces = keys
            .iter()
            .map(|key| key.normalized_surface.as_str())
            .collect::<Vec<_>>();
        sqlx::query(
            r#"
            WITH requested AS MATERIALIZED (
                SELECT language, dialect_scope, normalized_surface
                FROM unnest($1::text[], $2::text[], $3::text[])
                    AS value(language, dialect_scope, normalized_surface)
                ORDER BY language, dialect_scope, normalized_surface
            )
            SELECT pg_advisory_xact_lock(hashtextextended(
                'lexicon.surface:' || requested.language || ':' ||
                requested.dialect_scope || ':' || requested.normalized_surface,
                0
            ))
            FROM requested
            "#,
        )
        .bind(languages)
        .bind(dialect_scopes)
        .bind(normalized_surfaces)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        Ok(())
    }

    pub(crate) async fn replace_surface_projection(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        source_revision: i64,
        scope: SurfaceContentScope,
        previous_publication_id: Option<Uuid>,
        previous_sources: &[SurfaceProjectionSource],
        sources: &[SurfaceProjectionSource],
    ) -> Result<i64, LexiconRepositoryError> {
        if previous_sources
            .iter()
            .chain(sources)
            .any(|source| source.entry_id != entry_id)
        {
            return Err(LexiconRepositoryError::Invariant(
                "surface projection source belongs to another entry",
            ));
        }
        if matches!(scope, SurfaceContentScope::Draft) && previous_publication_id.is_some() {
            return Err(LexiconRepositoryError::Invariant(
                "draft surface projection cannot bind a publication",
            ));
        }
        let event_offset = sqlx::query_scalar::<_, i64>(
            "SELECT nextval('lexicon.surface_projection_event_offset_seq')",
        )
        .fetch_one(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        sqlx::query(
            r#"
            UPDATE lexicon.surface_sources
            SET source_revision = $3,
                event_offset = $4,
                is_deleted = TRUE,
                updated_at = now()
            WHERE entry_id = $1
              AND content_scope = $2
              AND (source_revision, event_offset) <= ($3, $4)
            "#,
        )
        .bind(entry_id)
        .bind(scope.as_str())
        .bind(source_revision)
        .bind(event_offset)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        for source in previous_sources {
            upsert_surface_source(
                tx,
                source,
                source_revision,
                event_offset,
                scope.as_str(),
                previous_publication_id,
                true,
            )
            .await?;
        }
        for source in sources {
            upsert_surface_source(
                tx,
                source,
                source_revision,
                event_offset,
                scope.as_str(),
                scope.publication_id(),
                false,
            )
            .await?;
        }

        sqlx::query(
            r#"
            INSERT INTO platform.outbox_events (
                id, aggregate_type, aggregate_id, aggregate_revision,
                event_type, payload, occurred_at, available_at
            ) VALUES (
                $1, 'lexicon.surface_projection', $2, $3,
                'lexicon.surface_projection_replaced', $4, now(), now()
            )
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(entry_id)
        .bind(event_offset)
        .bind(serde_json::json!({
            "entry_id": entry_id,
            "content_scope": scope.as_str(),
            "publication_id": scope.publication_id(),
            "source_revision": source_revision,
            "event_offset": event_offset,
            "source_count": sources.len(),
        }))
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        Ok(event_offset)
    }
}

#[allow(clippy::too_many_arguments)]
async fn upsert_surface_source(
    tx: &mut Transaction<'_, Postgres>,
    source: &SurfaceProjectionSource,
    source_revision: i64,
    event_offset: i64,
    content_scope: &str,
    publication_id: Option<Uuid>,
    is_deleted: bool,
) -> Result<(), LexiconRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO lexicon.surface_sources (
            entry_id, source_id, source_kind, source_node_id, language,
            entry_kind, dialect, dialect_scope, surface,
            normalized_surface, normalization_version, source_revision,
            event_offset, is_deleted, content_scope, publication_id,
            pos_id, pos, form_type, updated_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
            $13, $14, $15, $16, $17, $18, $19, now()
        )
        ON CONFLICT (
            source_id, content_scope, dialect_scope, normalization_version
        ) DO UPDATE SET
            entry_id = EXCLUDED.entry_id,
            source_kind = EXCLUDED.source_kind,
            source_node_id = EXCLUDED.source_node_id,
            language = EXCLUDED.language,
            entry_kind = EXCLUDED.entry_kind,
            dialect = EXCLUDED.dialect,
            surface = EXCLUDED.surface,
            normalized_surface = EXCLUDED.normalized_surface,
            source_revision = EXCLUDED.source_revision,
            event_offset = EXCLUDED.event_offset,
            is_deleted = EXCLUDED.is_deleted,
            publication_id = EXCLUDED.publication_id,
            pos_id = EXCLUDED.pos_id,
            pos = EXCLUDED.pos,
            form_type = EXCLUDED.form_type,
            updated_at = now()
        WHERE (
            lexicon.surface_sources.source_revision,
            lexicon.surface_sources.event_offset
        ) <= (EXCLUDED.source_revision, EXCLUDED.event_offset)
        "#,
    )
    .bind(source.entry_id)
    .bind(&source.source_id)
    .bind(source.source_kind)
    .bind(source.source_node_id)
    .bind(&source.language)
    .bind(source.entry_kind)
    .bind(source.dialect)
    .bind(source.dialect_scope)
    .bind(&source.surface)
    .bind(&source.normalized_surface)
    .bind(source.normalization_version)
    .bind(source_revision)
    .bind(event_offset)
    .bind(is_deleted)
    .bind(content_scope)
    .bind(publication_id)
    .bind(source.pos_id)
    .bind(&source.pos)
    .bind(&source.form_type)
    .execute(&mut **tx)
    .await
    .map_err(LexiconRepositoryError::Database)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::lexicon::dto::{
        AdminWordStatus, DialectRulesV2, DraftMeaningsStepContent, PersistedWordStep, TextOrigin,
        WordBaseFormSlotV2, WordCreationStep, WordDerivedFormSlotV2, WordDetectionSnapshotV2,
        WordFormGroupV2, WordPosFormsV2,
    };

    fn word(kind: EntryKind) -> AdminWordV2 {
        let id = Uuid::now_v7();
        let pos_id = Uuid::now_v7();
        let base_id = Uuid::now_v7();
        let plural_id = Uuid::now_v7();
        let now = Utc::now();
        AdminWordV2 {
            schema_version: 2,
            id,
            language: "en".to_owned(),
            kind,
            status: AdminWordStatus::Draft,
            revision: 2,
            lifecycle_revision: 1,
            published_revision: None,
            has_unpublished_changes: false,
            headwords: WordHeadwordsV2::Unified {
                common: "workspace".to_owned(),
            },
            frequency: None,
            detection_snapshot: WordDetectionSnapshotV2 {
                detection_id: Uuid::now_v7(),
                request: crate::lexicon::dto::DetectionRequestEcho {
                    language: "en".to_owned(),
                    headword: "workspace".to_owned(),
                },
                normalized_headword: "workspace".to_owned(),
                entry_kind: kind,
                matched_dialect: Dialect::Common,
                builtin_dictionary_status: "matched".to_owned(),
                smart_dictionary:
                    crate::lexicon::dto::WordDetectionSnapshotSmartDictionaryV2::Clear {
                        surface_warning: None,
                    },
                headwords: WordHeadwordsV2::Unified {
                    common: "workspace".to_owned(),
                },
                suggested_pos: vec!["noun".to_owned()],
                dictionary_provider: None,
                dictionary_coverage: None,
                dictionary_provenance: None,
                detected_at: now,
            },
            forms: DraftFormsStepContent {
                pos: vec![WordPosFormsV2 {
                    pos_id,
                    pos: "noun".to_owned(),
                    dialect_rules: DialectRulesV2 {
                        spelling_mode: "common".to_owned(),
                        phonetic_mode: "common".to_owned(),
                    },
                    base_form: WordBaseFormSlotV2 {
                        id: Uuid::now_v7(),
                        form_type: "base".to_owned(),
                        variants: vec![WordFormVariantV2 {
                            id: base_id,
                            dialect: Dialect::Common,
                            spelling: "workspace".to_owned(),
                            origin: TextOrigin::Manual,
                            pronunciations: Vec::new(),
                        }],
                    },
                    form_groups: vec![WordFormGroupV2 {
                        id: Uuid::now_v7(),
                        is_regular: true,
                        slots: vec![WordDerivedFormSlotV2 {
                            id: Uuid::now_v7(),
                            form_type: "plural".to_owned(),
                            variants: vec![WordFormVariantV2 {
                                id: plural_id,
                                dialect: Dialect::Common,
                                spelling: "workspaces".to_owned(),
                                origin: TextOrigin::Manual,
                                pronunciations: Vec::new(),
                            }],
                        }],
                    }],
                }],
            },
            meanings: DraftMeaningsStepContent::default(),
            completed_steps: vec![PersistedWordStep::Basics],
            max_reachable_step: WordCreationStep::Forms,
            created_by: Uuid::now_v7(),
            created_at: now,
            updated_at: now,
            archived_at: None,
            archived_by: None,
            published_at: None,
        }
    }

    #[test]
    fn derives_only_explicit_headword_and_form_surfaces_with_stable_sources() {
        let word = word(EntryKind::Word);
        let sources = surface_projection_sources(&word).unwrap();

        assert_eq!(sources.len(), 6, "three common sources expand to UK and US");
        assert_eq!(
            sources
                .iter()
                .filter(|source| source.normalized_surface == "workspaces")
                .count(),
            2
        );
        assert!(
            sources
                .iter()
                .all(|source| { source.source_id.starts_with(&format!("entry:{}:", word.id)) })
        );
        assert!(sources.iter().all(|source| {
            source.normalized_surface == "workspace" || source.normalized_surface == "workspaces"
        }));
    }

    #[test]
    fn lock_keys_are_kind_independent_sorted_and_deduplicated() {
        let word_sources = surface_projection_sources(&word(EntryKind::Word)).unwrap();
        let phrase_sources = surface_projection_sources(&word(EntryKind::Phrase)).unwrap();
        let keys = surface_lock_keys([word_sources.as_slice(), phrase_sources.as_slice()]);

        assert_eq!(keys.len(), 4);
        assert_eq!(keys[0].dialect_scope, "uk");
        assert_eq!(keys[0].normalized_surface, "workspace");
        assert_eq!(keys[1].normalized_surface, "workspaces");
        assert_eq!(keys[2].dialect_scope, "us");
        assert_eq!(keys[3].normalized_surface, "workspaces");
    }

    #[test]
    fn distinct_form_nodes_remain_distinct_sources_even_for_same_spelling() {
        let mut word = word(EntryKind::Word);
        let duplicate = word.forms.pos[0].form_groups[0].slots[0].variants[0].clone();
        let mut second = duplicate;
        second.id = Uuid::now_v7();
        word.forms.pos[0].form_groups[0].slots[0]
            .variants
            .push(second);

        let sources = surface_projection_sources(&word).unwrap();
        let plural_source_ids = sources
            .iter()
            .filter(|source| source.normalized_surface == "workspaces")
            .map(|source| source.source_id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(plural_source_ids.len(), 2);
    }
}
