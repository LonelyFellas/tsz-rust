DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon.sentence_associations
        WHERE segment_count > 1
    ) THEN
        RAISE EXCEPTION 'cannot remove sentence association segments while multi-segment data exists'
            USING ERRCODE = '0A000';
    END IF;
END
$$;

DROP TRIGGER lexicon_sentence_associations_legacy_segment_trigger
    ON lexicon.sentence_associations;
DROP FUNCTION lexicon.sync_legacy_sentence_association_segment();
DROP TABLE lexicon.sentence_association_segments;

ALTER TABLE lexicon.sentence_associations
    DROP CONSTRAINT lexicon_sentence_associations_segment_parent_key;
