-- Expand phase for V3 multi-segment association positions. Existing V2 rows
-- remain single-range writers and are dual-written by a trigger until cutover.

ALTER TABLE lexicon.sentence_associations
    ADD CONSTRAINT lexicon_sentence_associations_segment_parent_key
        UNIQUE (id, sentence_id, source_dialect);

CREATE TABLE lexicon.sentence_association_segments (
    association_id UUID NOT NULL,
    ordinal SMALLINT NOT NULL
        CONSTRAINT lexicon_sentence_association_segments_ordinal_check
        CHECK (ordinal BETWEEN 0 AND 19),
    sentence_id UUID NOT NULL,
    source_dialect TEXT NOT NULL
        CONSTRAINT lexicon_sentence_association_segments_dialect_check
        CHECK (source_dialect IN ('common', 'uk', 'us')),
    range_start INTEGER NOT NULL
        CONSTRAINT lexicon_sentence_association_segments_start_check
        CHECK (range_start >= 0),
    range_end INTEGER NOT NULL
        CONSTRAINT lexicon_sentence_association_segments_end_check
        CHECK (range_end > range_start),
    surface TEXT NOT NULL
        CONSTRAINT lexicon_sentence_association_segments_surface_check
        CHECK (
            surface = btrim(surface)
            AND char_length(surface) BETWEEN 1 AND 200
        ),
    PRIMARY KEY (association_id, ordinal),
    CONSTRAINT lexicon_sentence_association_segments_parent_fkey
        FOREIGN KEY (association_id, sentence_id, source_dialect)
        REFERENCES lexicon.sentence_associations (id, sentence_id, source_dialect)
        ON DELETE CASCADE
);

CREATE INDEX lexicon_sentence_association_segments_source_idx
    ON lexicon.sentence_association_segments (
        sentence_id,
        source_dialect,
        range_start,
        range_end
    );

INSERT INTO lexicon.sentence_association_segments (
    association_id,
    ordinal,
    sentence_id,
    source_dialect,
    range_start,
    range_end,
    surface
)
SELECT id, 0, sentence_id, source_dialect, range_start, range_end, surface
FROM lexicon.sentence_associations;

CREATE FUNCTION lexicon.sync_legacy_sentence_association_segment()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.association_schema_version = 2 THEN
        INSERT INTO lexicon.sentence_association_segments (
            association_id,
            ordinal,
            sentence_id,
            source_dialect,
            range_start,
            range_end,
            surface
        ) VALUES (
            NEW.id,
            0,
            NEW.sentence_id,
            NEW.source_dialect,
            NEW.range_start,
            NEW.range_end,
            NEW.surface
        )
        ON CONFLICT (association_id, ordinal) DO UPDATE
        SET sentence_id = EXCLUDED.sentence_id,
            source_dialect = EXCLUDED.source_dialect,
            range_start = EXCLUDED.range_start,
            range_end = EXCLUDED.range_end,
            surface = EXCLUDED.surface;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER lexicon_sentence_associations_legacy_segment_trigger
AFTER INSERT OR UPDATE OF sentence_id, source_dialect, range_start, range_end, surface,
    association_schema_version
ON lexicon.sentence_associations
FOR EACH ROW
EXECUTE FUNCTION lexicon.sync_legacy_sentence_association_segment();
