-- V3 projections are rebuildable, but silently deleting them during rollback would
-- hide an unsafe downgrade. Require the caller to disable writers and clear/rebuild
-- derived V3 projection rows explicitly before applying this down migration.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon.surface_sources
        WHERE content_schema_version = 3
    ) OR EXISTS (
        SELECT 1
        FROM lexicon.entry_presentation_projection
    ) THEN
        RAISE EXCEPTION
            'cannot contract Smart Lexicon V3 projections while V3 projection rows exist';
    END IF;
END
$$;

DROP TABLE lexicon.entry_presentation_projection;
DROP INDEX lexicon.lexicon_surface_sources_v3_lookup_idx;

ALTER TABLE lexicon.surface_sources
    DROP CONSTRAINT lexicon_surface_sources_schema_version_check,
    DROP CONSTRAINT lexicon_surface_sources_kind_check,
    DROP CONSTRAINT lexicon_surface_sources_source_shape_check;

ALTER TABLE lexicon.surface_sources
    DROP COLUMN content_schema_version,
    DROP COLUMN form_id,
    DROP COLUMN variant_id,
    DROP COLUMN group_ids,
    DROP COLUMN projection_version;

ALTER TABLE lexicon.surface_sources
    ADD CONSTRAINT lexicon_surface_sources_kind_check
        CHECK (source_kind IN ('headword', 'form')),
    ADD CONSTRAINT lexicon_surface_sources_source_shape_check CHECK (
        (
            source_kind = 'headword'
            AND source_node_id IS NULL
            AND pos_id IS NULL
            AND pos IS NULL
            AND form_type IS NULL
        )
        OR
        (
            source_kind = 'form'
            AND source_node_id IS NOT NULL
            AND pos_id IS NOT NULL
            AND pos IS NOT NULL
            AND btrim(pos) <> ''
            AND form_type IN (
                'base', 'present_participle', 'past_tense', 'past_participle',
                'third_person_singular', 'plural', 'comparative', 'superlative'
            )
        )
    );
