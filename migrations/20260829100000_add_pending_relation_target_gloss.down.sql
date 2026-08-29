ALTER TABLE lexicon.relations
    DROP CONSTRAINT lexicon_relations_target_shape_check,
    DROP COLUMN pending_target_gloss;

ALTER TABLE lexicon.relations
    ADD CONSTRAINT lexicon_relations_target_shape_check CHECK (
        (
            target_entry_id IS NOT NULL
            AND target_sense_id IS NOT NULL
            AND target_headword_snapshot IS NOT NULL
            AND target_gloss_snapshot IS NOT NULL
            AND pending_target_headword IS NULL
        )
        OR (
            target_entry_id IS NULL
            AND target_sense_id IS NULL
            AND target_headword_snapshot IS NULL
            AND target_gloss_snapshot IS NULL
            AND pending_target_headword IS NOT NULL
            AND pending_target_headword = btrim(pending_target_headword)
            AND char_length(pending_target_headword) BETWEEN 1 AND 200
        )
    );
