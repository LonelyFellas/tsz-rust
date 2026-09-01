-- up 阶段已把预绑定行的 pending_target_headword 清空，旧约束要求它非空，
-- 无法凭空还原被清的数据；仿照 20260830080000 down 的守卫风格，存在预绑定行时拒绝回滚。
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon.relations
        WHERE prebound_target_entry_id IS NOT NULL
    ) THEN
        RAISE EXCEPTION
            'cannot restore the wide prebound relation shape while prebound relations exist';
    END IF;
END
$$;

ALTER TABLE lexicon.relations
    DROP CONSTRAINT lexicon_relations_target_shape_check,
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
