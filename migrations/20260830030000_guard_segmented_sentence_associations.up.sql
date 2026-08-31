-- Guard release metadata for the future segmented V3 association contract.
-- This migration does not create or write child segments. It only makes it
-- possible for an old `source_range` writer to fail closed once segmented
-- data exists after the later expand/cutover releases.

ALTER TABLE lexicon.sentence_associations
    ADD COLUMN association_schema_version SMALLINT NOT NULL DEFAULT 2,
    ADD COLUMN segment_count SMALLINT NOT NULL DEFAULT 1,
    ADD COLUMN segments_fingerprint BYTEA;

ALTER TABLE lexicon.sentence_associations
    ADD CONSTRAINT lexicon_sentence_associations_schema_version_check
        CHECK (association_schema_version IN (2, 3)),
    ADD CONSTRAINT lexicon_sentence_associations_segment_count_check
        CHECK (segment_count BETWEEN 1 AND 20),
    ADD CONSTRAINT lexicon_sentence_associations_v2_segment_count_check
        CHECK (association_schema_version = 3 OR segment_count = 1);

CREATE INDEX lexicon_sentence_associations_segmented_guard_idx
    ON lexicon.sentence_associations (sentence_id)
    WHERE segment_count > 1;
