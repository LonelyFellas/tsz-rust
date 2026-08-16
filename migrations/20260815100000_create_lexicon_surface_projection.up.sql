-- Expand-only surface lookup projection for smart lexicon warnings.
--
-- This migration deliberately leaves
-- lexicon_entry_headword_keys_unique_idx in place.  Removing that legacy
-- cross-entry guard is the separately reviewed B4 cutover.
CREATE SEQUENCE lexicon.surface_projection_event_offset_seq AS BIGINT;

CREATE TABLE lexicon.surface_sources (
    entry_id UUID NOT NULL
        CONSTRAINT lexicon_surface_sources_entry_fkey
        REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    -- Stable within an entry/content source. Headwords use a stable logical
    -- identifier (entry + headword slot); forms use their stable variant node.
    source_id TEXT NOT NULL,
    source_kind TEXT NOT NULL
        CONSTRAINT lexicon_surface_sources_kind_check
        CHECK (source_kind IN ('headword', 'form')),
    source_node_id UUID,
    language TEXT NOT NULL
        CONSTRAINT lexicon_surface_sources_language_check CHECK (language = 'en'),
    entry_kind TEXT NOT NULL
        CONSTRAINT lexicon_surface_sources_entry_kind_check
        CHECK (entry_kind IN ('word', 'phrase')),
    dialect TEXT NOT NULL
        CONSTRAINT lexicon_surface_sources_dialect_check
        CHECK (dialect IN ('common', 'uk', 'us')),
    dialect_scope TEXT NOT NULL
        CONSTRAINT lexicon_surface_sources_dialect_scope_check
        CHECK (dialect_scope IN ('uk', 'us')),
    surface TEXT NOT NULL,
    normalized_surface TEXT NOT NULL,
    normalization_version SMALLINT NOT NULL
        CONSTRAINT lexicon_surface_sources_normalization_version_check
        CHECK (normalization_version > 0),
    source_revision BIGINT NOT NULL
        CONSTRAINT lexicon_surface_sources_revision_check CHECK (source_revision > 0),
    event_offset BIGINT NOT NULL
        DEFAULT nextval('lexicon.surface_projection_event_offset_seq')
        CONSTRAINT lexicon_surface_sources_event_offset_check CHECK (event_offset > 0),
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    content_scope TEXT NOT NULL
        CONSTRAINT lexicon_surface_sources_content_scope_check
        CHECK (content_scope IN ('draft', 'current_publication')),
    publication_id UUID,
    pos_id UUID,
    pos TEXT,
    form_type TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (
        source_id,
        content_scope,
        dialect_scope,
        normalization_version
    ),
    CONSTRAINT lexicon_surface_sources_source_id_nonempty_check CHECK (
        source_id = btrim(source_id) AND source_id <> ''
    ),
    CONSTRAINT lexicon_surface_sources_surface_nonempty_check CHECK (
        surface = btrim(surface)
        AND normalized_surface = btrim(normalized_surface)
        AND char_length(surface) BETWEEN 1 AND 200
        AND char_length(normalized_surface) BETWEEN 1 AND 200
    ),
    CONSTRAINT lexicon_surface_sources_source_shape_check CHECK (
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
    ),
    CONSTRAINT lexicon_surface_sources_publication_shape_check CHECK (
        (content_scope = 'draft' AND publication_id IS NULL)
        OR (content_scope = 'current_publication' AND publication_id IS NOT NULL)
    ),
    CONSTRAINT lexicon_surface_sources_publication_fkey
        FOREIGN KEY (publication_id, entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id)
        ON DELETE CASCADE
);

-- Non-unique by design: different entries and kinds may share a surface.
CREATE INDEX lexicon_surface_sources_lookup_idx
    ON lexicon.surface_sources (
        language,
        dialect_scope,
        normalized_surface,
        entry_kind,
        entry_id,
        source_kind,
        source_id,
        content_scope
    )
    WHERE is_deleted = FALSE;

CREATE INDEX lexicon_surface_sources_entry_idx
    ON lexicon.surface_sources (
        entry_id,
        content_scope,
        source_revision,
        event_offset
    );

CREATE INDEX lexicon_entry_headword_keys_lookup_idx
    ON lexicon.entry_headword_keys (
        language,
        dialect_scope,
        normalized_headword,
        kind,
        entry_id
    );
