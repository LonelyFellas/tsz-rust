-- Expand-only storage for Smart Lexicon V3 surface and presentation projections.
-- Existing writers omit the new columns and therefore continue producing strict V2 rows.
ALTER TABLE lexicon.surface_sources
    ADD COLUMN content_schema_version SMALLINT NOT NULL DEFAULT 2,
    ADD COLUMN form_id UUID,
    ADD COLUMN variant_id UUID,
    ADD COLUMN group_ids UUID[],
    ADD COLUMN projection_version TEXT;

ALTER TABLE lexicon.surface_sources
    DROP CONSTRAINT lexicon_surface_sources_kind_check,
    DROP CONSTRAINT lexicon_surface_sources_source_shape_check;

ALTER TABLE lexicon.surface_sources
    ADD CONSTRAINT lexicon_surface_sources_schema_version_check
        CHECK (content_schema_version IN (2, 3)),
    ADD CONSTRAINT lexicon_surface_sources_kind_check
        CHECK (source_kind IN ('headword', 'form', 'form_variant')),
    ADD CONSTRAINT lexicon_surface_sources_source_shape_check CHECK (
        (
            content_schema_version = 2
            AND source_kind IN ('headword', 'form')
            AND form_id IS NULL
            AND variant_id IS NULL
            AND group_ids IS NULL
            AND projection_version IS NULL
            AND (
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
                    AND form_type IS NOT NULL
                    AND form_type IN (
                        'base', 'present_participle', 'past_tense', 'past_participle',
                        'third_person_singular', 'plural', 'comparative', 'superlative'
                    )
                )
            )
        )
        OR
        (
            content_schema_version = 3
            AND source_kind = 'form_variant'
            AND source_node_id IS NOT NULL
            AND source_node_id = variant_id
            AND pos_id IS NOT NULL
            AND pos IS NOT NULL
            AND btrim(pos) <> ''
            AND form_id IS NOT NULL
            AND variant_id IS NOT NULL
            AND group_ids IS NOT NULL
            AND cardinality(group_ids) > 0
            AND array_position(group_ids, NULL) IS NULL
            AND projection_version IS NOT NULL
            AND projection_version = btrim(projection_version)
            AND char_length(projection_version) BETWEEN 1 AND 100
            AND form_type IS NOT NULL
            AND form_type IN (
                'base', 'present_participle', 'past_tense', 'past_participle',
                'third_person_singular', 'plural', 'comparative', 'superlative'
            )
            AND (
                (dialect = 'common' AND dialect_scope IN ('uk', 'us'))
                OR (dialect = 'uk' AND dialect_scope = 'uk')
                OR (dialect = 'us' AND dialect_scope = 'us')
            )
        )
    );

-- Deliberately non-unique: one normalized surface may identify multiple entries,
-- forms and regional variants. Identity remains entry/form/variant UUID based.
CREATE INDEX lexicon_surface_sources_v3_lookup_idx
    ON lexicon.surface_sources (
        language,
        dialect_scope,
        normalized_surface,
        entry_kind,
        entry_id,
        form_id,
        variant_id,
        content_scope
    )
    WHERE content_schema_version = 3
      AND source_kind = 'form_variant'
      AND is_deleted = FALSE;

CREATE TABLE lexicon.entry_presentation_projection (
    entry_id UUID PRIMARY KEY
        CONSTRAINT lexicon_entry_presentation_projection_entry_fkey
        REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    content_schema_version SMALLINT NOT NULL
        CONSTRAINT lexicon_entry_presentation_projection_schema_version_check
        CHECK (content_schema_version = 3),
    source_revision BIGINT NOT NULL
        CONSTRAINT lexicon_entry_presentation_projection_revision_check
        CHECK (source_revision > 0),
    label TEXT NOT NULL,
    matched_surfaces TEXT[] NOT NULL,
    strategy_version TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT lexicon_entry_presentation_projection_label_check CHECK (
        label = btrim(label) AND label <> ''
    ),
    CONSTRAINT lexicon_entry_presentation_projection_surfaces_check CHECK (
        cardinality(matched_surfaces) <= 2000
        AND array_position(matched_surfaces, NULL) IS NULL
    ),
    CONSTRAINT lexicon_entry_presentation_projection_strategy_check CHECK (
        strategy_version = btrim(strategy_version)
        AND char_length(strategy_version) BETWEEN 1 AND 100
    )
);

CREATE INDEX lexicon_entry_presentation_projection_revision_idx
    ON lexicon.entry_presentation_projection (source_revision, entry_id);
