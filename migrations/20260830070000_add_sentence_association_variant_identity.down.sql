DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon.sentence_associations
        WHERE target_publication_id IS NOT NULL
           OR target_form_variant_id IS NOT NULL
           OR target_component_usages_snapshot IS NOT NULL
    ) THEN
        RAISE EXCEPTION 'cannot remove sentence association variant identity while data exists'
            USING ERRCODE = '0A000';
    END IF;
END
$$;

DROP INDEX lexicon.lexicon_sentence_associations_target_variant_idx;

ALTER TABLE lexicon.sentence_associations
    DROP CONSTRAINT lexicon_sentence_associations_target_publication_form_fkey,
    DROP CONSTRAINT lexicon_sentence_associations_target_publication_sense_fkey,
    DROP CONSTRAINT lexicon_sentence_associations_target_publication_variant_fkey,
    DROP CONSTRAINT lexicon_sentence_associations_target_variant_fkey,
    DROP CONSTRAINT lexicon_sentence_associations_target_publication_fkey,
    DROP CONSTRAINT lexicon_sentence_associations_variant_identity_shape_check,
    DROP COLUMN target_form_variant_id,
    DROP COLUMN target_publication_id,
    DROP COLUMN target_component_usages_snapshot;
