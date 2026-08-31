DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon.relations
        WHERE prebound_target_entry_id IS NOT NULL
           OR prebinding_reason IS NOT NULL
    ) THEN
        RAISE EXCEPTION
            'cannot remove draft relation prebinding while prebound relations exist';
    END IF;
END
$$;

DROP INDEX lexicon.lexicon_relations_prebound_target_idx;

ALTER TABLE lexicon.relations
    DROP CONSTRAINT lexicon_relations_target_shape_check,
    DROP CONSTRAINT lexicon_relations_prebound_not_self_check,
    DROP CONSTRAINT lexicon_relations_prebinding_reason_check,
    DROP CONSTRAINT lexicon_relations_prebound_target_fkey,
    DROP COLUMN prebinding_reason,
    DROP COLUMN prebound_target_entry_id;

ALTER TABLE lexicon.relations
    ADD CONSTRAINT lexicon_relations_target_shape_check CHECK (
        (
            target_entry_id IS NOT NULL
            AND target_sense_id IS NOT NULL
            AND target_headword_snapshot IS NOT NULL
            AND target_gloss_snapshot IS NOT NULL
            AND pending_target_headword IS NULL
            AND pending_target_gloss IS NULL
        )
        OR (
            target_entry_id IS NULL
            AND target_sense_id IS NULL
            AND target_headword_snapshot IS NULL
            AND target_gloss_snapshot IS NULL
            AND pending_target_headword IS NOT NULL
            AND pending_target_headword = btrim(pending_target_headword)
            AND char_length(pending_target_headword) BETWEEN 1 AND 200
            AND (
                pending_target_gloss IS NULL
                OR (
                    pending_target_gloss = btrim(pending_target_gloss)
                    AND char_length(pending_target_gloss) BETWEEN 1 AND 5000
                )
            )
        )
    );
