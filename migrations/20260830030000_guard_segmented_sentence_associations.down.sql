DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon.sentence_associations
        WHERE segment_count > 1
    ) THEN
        RAISE EXCEPTION 'cannot remove segmented sentence association guard while multi-segment data exists'
            USING ERRCODE = '0A000';
    END IF;
END
$$;

DROP INDEX lexicon.lexicon_sentence_associations_segmented_guard_idx;

ALTER TABLE lexicon.sentence_associations
    DROP CONSTRAINT lexicon_sentence_associations_v2_segment_count_check,
    DROP CONSTRAINT lexicon_sentence_associations_segment_count_check,
    DROP CONSTRAINT lexicon_sentence_associations_schema_version_check,
    DROP COLUMN segments_fingerprint,
    DROP COLUMN segment_count,
    DROP COLUMN association_schema_version;
