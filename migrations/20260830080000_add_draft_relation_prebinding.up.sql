ALTER TABLE lexicon.relations
    DROP CONSTRAINT lexicon_relations_target_shape_check,
    ADD COLUMN prebound_target_entry_id UUID,
    ADD COLUMN prebinding_reason TEXT;

ALTER TABLE lexicon.relations
    ADD CONSTRAINT lexicon_relations_prebound_target_fkey
        FOREIGN KEY (prebound_target_entry_id)
        REFERENCES lexicon.entries(id)
        ON DELETE RESTRICT,
    ADD CONSTRAINT lexicon_relations_prebinding_reason_check CHECK (
        prebinding_reason IS NULL
        OR prebinding_reason IN ('waiting_first_sense', 'target_sense_deleted')
    ),
    ADD CONSTRAINT lexicon_relations_prebound_not_self_check CHECK (
        prebound_target_entry_id IS NULL
        OR prebound_target_entry_id <> entry_id
    ),
    ADD CONSTRAINT lexicon_relations_target_shape_check CHECK (
        (
            target_entry_id IS NOT NULL
            AND target_sense_id IS NOT NULL
            AND target_headword_snapshot IS NOT NULL
            AND target_gloss_snapshot IS NOT NULL
            AND prebound_target_entry_id IS NULL
            AND prebinding_reason IS NULL
            AND pending_target_headword IS NULL
            AND pending_target_gloss IS NULL
        )
        OR (
            target_entry_id IS NULL
            AND target_sense_id IS NULL
            AND target_headword_snapshot IS NULL
            AND target_gloss_snapshot IS NULL
            AND prebound_target_entry_id IS NULL
            AND prebinding_reason IS NULL
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
        OR (
            target_entry_id IS NULL
            AND target_sense_id IS NULL
            AND target_headword_snapshot IS NULL
            AND target_gloss_snapshot IS NULL
            AND prebound_target_entry_id IS NOT NULL
            AND prebinding_reason IS NOT NULL
            AND prebinding_reason IN ('waiting_first_sense', 'target_sense_deleted')
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

CREATE INDEX lexicon_relations_prebound_target_idx
    ON lexicon.relations (
        prebound_target_entry_id,
        prebinding_reason,
        entry_id,
        id
    )
    WHERE prebound_target_entry_id IS NOT NULL;
