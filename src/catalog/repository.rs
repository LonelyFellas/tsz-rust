use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    catalog::model::{
        CatalogFlatRecord, NewPart, NewSubPart, PartChanges, PartListFilter, PartRecord,
        SubPartChanges, SubPartRecord,
    },
    platform::{is_foreign_key_violation, is_unique_violation},
};

mod commands;
mod query;

const PART_LIST_SQL: &str = r#"
    SELECT p.id, p.code, p.name_zh, p.name_en, p.abbreviation, p.sort_order, p.revision,
           p.created_by_admin_id, creator.display_name AS created_by_display_name,
           p.updated_by_admin_id, updater.display_name AS updated_by_display_name,
           p.created_at, p.updated_at,
           (
               SELECT count(*)::bigint
               FROM (
                   SELECT draft.entry_id
                   FROM lexicon.entry_pos draft
                   WHERE draft.part_of_speech_id = p.id
                   UNION
                   SELECT publication.entry_id
                   FROM lexicon.entry_publication_part_of_speech_refs publication
                   WHERE publication.part_of_speech_id = p.id
                   UNION
                   SELECT draft_sense.entry_id
                   FROM lexicon.senses draft_sense
                   JOIN catalog.sub_parts_of_speech used_sub
                     ON used_sub.id = draft_sense.sub_part_of_speech_id
                   WHERE used_sub.part_of_speech_id = p.id
                   UNION
                   SELECT publication_sense.entry_id
                   FROM lexicon.entry_publication_sub_part_of_speech_refs publication_sense
                   JOIN catalog.sub_parts_of_speech used_sub
                     ON used_sub.id = publication_sense.sub_part_of_speech_id
                   WHERE used_sub.part_of_speech_id = p.id
               ) usage_entries
           ) AS usage_count,
           (
               SELECT count(*)::bigint
               FROM catalog.sub_parts_of_speech child
               WHERE child.part_of_speech_id = p.id
           ) AS sub_part_count
    FROM catalog.parts_of_speech p
    LEFT JOIN admins creator ON creator.id = p.created_by_admin_id
    LEFT JOIN admins updater ON updater.id = p.updated_by_admin_id
    WHERE $1::text IS NULL
       OR strpos(lower(p.code), lower($1)) > 0
       OR strpos(lower(p.name_zh), lower($1)) > 0
       OR strpos(lower(p.name_en), lower($1)) > 0
       OR strpos(lower(p.abbreviation), lower($1)) > 0
    ORDER BY p.sort_order, p.created_at, p.id
    LIMIT $2 OFFSET $3
"#;

const PART_BY_ID_SQL: &str = r#"
    SELECT p.id, p.code, p.name_zh, p.name_en, p.abbreviation, p.sort_order, p.revision,
           p.created_by_admin_id, creator.display_name AS created_by_display_name,
           p.updated_by_admin_id, updater.display_name AS updated_by_display_name,
           p.created_at, p.updated_at,
           (
               SELECT count(*)::bigint
               FROM (
                   SELECT draft.entry_id
                   FROM lexicon.entry_pos draft
                   WHERE draft.part_of_speech_id = p.id
                   UNION
                   SELECT publication.entry_id
                   FROM lexicon.entry_publication_part_of_speech_refs publication
                   WHERE publication.part_of_speech_id = p.id
                   UNION
                   SELECT draft_sense.entry_id
                   FROM lexicon.senses draft_sense
                   JOIN catalog.sub_parts_of_speech used_sub
                     ON used_sub.id = draft_sense.sub_part_of_speech_id
                   WHERE used_sub.part_of_speech_id = p.id
                   UNION
                   SELECT publication_sense.entry_id
                   FROM lexicon.entry_publication_sub_part_of_speech_refs publication_sense
                   JOIN catalog.sub_parts_of_speech used_sub
                     ON used_sub.id = publication_sense.sub_part_of_speech_id
                   WHERE used_sub.part_of_speech_id = p.id
               ) usage_entries
           ) AS usage_count,
           (
               SELECT count(*)::bigint
               FROM catalog.sub_parts_of_speech child
               WHERE child.part_of_speech_id = p.id
           ) AS sub_part_count
    FROM catalog.parts_of_speech p
    LEFT JOIN admins creator ON creator.id = p.created_by_admin_id
    LEFT JOIN admins updater ON updater.id = p.updated_by_admin_id
    WHERE p.id = $1
"#;

const SUB_PART_LIST_SQL: &str = r#"
    SELECT s.id, s.part_of_speech_id, s.code, s.name_zh, s.name_en, s.sort_order, s.revision,
           s.created_by_admin_id, creator.display_name AS created_by_display_name,
           s.updated_by_admin_id, updater.display_name AS updated_by_display_name,
           s.created_at, s.updated_at,
           (
               SELECT count(*)::bigint
               FROM (
                   SELECT draft.id AS source_node_id
                   FROM lexicon.senses draft
                   WHERE draft.sub_part_of_speech_id = s.id
                   UNION
                   SELECT publication.source_node_id
                   FROM lexicon.entry_publication_sub_part_of_speech_refs publication
                   WHERE publication.sub_part_of_speech_id = s.id
               ) usage_senses
           ) AS usage_count
    FROM catalog.sub_parts_of_speech s
    LEFT JOIN admins creator ON creator.id = s.created_by_admin_id
    LEFT JOIN admins updater ON updater.id = s.updated_by_admin_id
    WHERE s.part_of_speech_id = $1
    ORDER BY s.sort_order, s.created_at, s.id
"#;

const SUB_PART_BY_ID_SQL: &str = r#"
    SELECT s.id, s.part_of_speech_id, s.code, s.name_zh, s.name_en, s.sort_order, s.revision,
           s.created_by_admin_id, creator.display_name AS created_by_display_name,
           s.updated_by_admin_id, updater.display_name AS updated_by_display_name,
           s.created_at, s.updated_at,
           (
               SELECT count(*)::bigint
               FROM (
                   SELECT draft.id AS source_node_id
                   FROM lexicon.senses draft
                   WHERE draft.sub_part_of_speech_id = s.id
                   UNION
                   SELECT publication.source_node_id
                   FROM lexicon.entry_publication_sub_part_of_speech_refs publication
                   WHERE publication.sub_part_of_speech_id = s.id
               ) usage_senses
           ) AS usage_count
    FROM catalog.sub_parts_of_speech s
    LEFT JOIN admins creator ON creator.id = s.created_by_admin_id
    LEFT JOIN admins updater ON updater.id = s.updated_by_admin_id
    WHERE s.part_of_speech_id = $1 AND s.id = $2
"#;

#[derive(Debug, thiserror::Error)]
pub enum CatalogRepositoryError {
    #[error("part of speech conflicts on {0}")]
    PartConflict(&'static str),
    #[error("sub part of speech conflicts on {0}")]
    SubPartConflict(&'static str),
    #[error("part of speech is in use")]
    PartInUse,
    #[error("sub part of speech is in use")]
    SubPartInUse,
    #[error("parent part of speech no longer exists")]
    ParentNotFound,
    #[error("catalog invariant violated: {0}")]
    Invariant(&'static str),
    #[error("database operation failed")]
    Database(#[source] sqlx::Error),
}

pub struct CatalogRepository {
    pool: PgPool,
}

impl CatalogRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn map_part_write_error(error: sqlx::Error) -> CatalogRepositoryError {
    for (constraint, field) in [
        ("catalog_parts_of_speech_code_unique_idx", "code"),
        ("catalog_parts_of_speech_name_zh_unique_idx", "name_zh"),
        ("catalog_parts_of_speech_name_en_unique_idx", "name_en"),
        (
            "catalog_parts_of_speech_abbreviation_unique_idx",
            "abbreviation",
        ),
    ] {
        if is_unique_violation(&error, constraint) {
            return CatalogRepositoryError::PartConflict(field);
        }
    }
    CatalogRepositoryError::Database(error)
}

fn map_sub_part_write_error(error: sqlx::Error) -> CatalogRepositoryError {
    for (constraint, field) in [
        ("catalog_sub_parts_code_unique_idx", "code"),
        ("catalog_sub_parts_name_zh_unique_idx", "name_zh"),
        ("catalog_sub_parts_name_en_unique_idx", "name_en"),
    ] {
        if is_unique_violation(&error, constraint) {
            return CatalogRepositoryError::SubPartConflict(field);
        }
    }
    if is_foreign_key_violation(&error, "catalog_sub_parts_parent_fkey") {
        return CatalogRepositoryError::ParentNotFound;
    }
    CatalogRepositoryError::Database(error)
}

fn map_part_delete_error(error: sqlx::Error) -> CatalogRepositoryError {
    for constraint in [
        "lexicon_entry_pos_catalog_pos_fkey",
        "lexicon_senses_catalog_sub_pos_fkey",
        "lexicon_publication_pos_refs_catalog_fkey",
        "lexicon_publication_sub_pos_refs_catalog_fkey",
    ] {
        if is_foreign_key_violation(&error, constraint) {
            return CatalogRepositoryError::PartInUse;
        }
    }
    CatalogRepositoryError::Database(error)
}

fn map_sub_part_delete_error(error: sqlx::Error) -> CatalogRepositoryError {
    for constraint in [
        "lexicon_senses_catalog_sub_pos_fkey",
        "lexicon_publication_sub_pos_refs_catalog_fkey",
    ] {
        if is_foreign_key_violation(&error, constraint) {
            return CatalogRepositoryError::SubPartInUse;
        }
    }
    CatalogRepositoryError::Database(error)
}
