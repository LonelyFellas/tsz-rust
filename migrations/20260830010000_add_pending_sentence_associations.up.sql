-- 例句人工关联增加 Pending 目标形态。现有 linked 行原样保留；Pending 仍锚定稳定
-- sentence node，继续独立于 meanings 内容表的整步重写。

ALTER TABLE lexicon.sentence_associations
    ADD COLUMN state TEXT NOT NULL DEFAULT 'linked',
    ADD COLUMN pending_target_kind TEXT,
    ADD COLUMN pending_target_headword TEXT,
    ADD COLUMN normalized_pending_target_headword TEXT,
    ADD COLUMN pending_target_gloss TEXT,
    ALTER COLUMN target_entry_id DROP NOT NULL,
    ALTER COLUMN target_sense_id DROP NOT NULL,
    ALTER COLUMN target_headword_snapshot DROP NOT NULL,
    ALTER COLUMN target_gloss_snapshot DROP NOT NULL,
    ALTER COLUMN resolved_pos DROP NOT NULL;

ALTER TABLE lexicon.sentence_associations
    ADD CONSTRAINT lexicon_sentence_associations_state_check
        CHECK (state IN ('linked', 'pending')),
    ADD CONSTRAINT lexicon_sentence_associations_pending_kind_check
        CHECK (pending_target_kind IS NULL OR pending_target_kind IN ('word', 'phrase')),
    ADD CONSTRAINT lexicon_sentence_associations_pending_headword_check
        CHECK (
            pending_target_headword IS NULL OR (
                pending_target_headword = btrim(pending_target_headword)
                AND char_length(pending_target_headword) BETWEEN 1 AND 200
            )
        ),
    ADD CONSTRAINT lexicon_sentence_associations_pending_normalized_check
        CHECK (
            normalized_pending_target_headword IS NULL OR (
                normalized_pending_target_headword = btrim(normalized_pending_target_headword)
                AND char_length(normalized_pending_target_headword) BETWEEN 1 AND 200
            )
        ),
    ADD CONSTRAINT lexicon_sentence_associations_pending_gloss_check
        CHECK (
            pending_target_gloss IS NULL OR (
                pending_target_gloss = btrim(pending_target_gloss)
                AND char_length(pending_target_gloss) BETWEEN 1 AND 5000
            )
        ),
    ADD CONSTRAINT lexicon_sentence_associations_target_shape_check
        CHECK (
            (
                state = 'linked'
                AND target_entry_id IS NOT NULL
                AND target_sense_id IS NOT NULL
                AND target_headword_snapshot IS NOT NULL
                AND target_gloss_snapshot IS NOT NULL
                AND resolved_pos IS NOT NULL
                AND pending_target_kind IS NULL
                AND pending_target_headword IS NULL
                AND normalized_pending_target_headword IS NULL
                AND pending_target_gloss IS NULL
            ) OR (
                state = 'pending'
                AND origin = 'manual'
                AND target_entry_id IS NULL
                AND target_sense_id IS NULL
                AND target_form_slot_id IS NULL
                AND target_headword_snapshot IS NULL
                AND target_gloss_snapshot IS NULL
                AND resolved_pos IS NULL
                AND resolved_form_type IS NULL
                AND pending_target_kind IS NOT NULL
                AND pending_target_headword IS NOT NULL
                AND normalized_pending_target_headword IS NOT NULL
            )
        );

CREATE INDEX lexicon_sentence_associations_pending_target_idx
    ON lexicon.sentence_associations (
        normalized_pending_target_headword,
        pending_target_kind,
        updated_at,
        id
    )
    WHERE state = 'pending';
