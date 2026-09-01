-- 预绑定关系不再携带待建词面：词条身份已由 prebound_target_entry_id 表达，
-- pending_target_headword 收窄回唯一含义「纯待建词条的词面」。预绑定行的词面
-- 回显改由服务端在合并阶段填充只读 target_headword（不落 snapshot 列）。
-- 该形态从未在测试服/生产启用（capability 门控），存量清理仅覆盖本地开发库。
-- 旧约束要求预绑定行词面非空，必须先卸约束再清数据，最后装新约束。
ALTER TABLE lexicon.relations
    DROP CONSTRAINT lexicon_relations_target_shape_check;

UPDATE lexicon.relations
SET pending_target_headword = NULL
WHERE prebound_target_entry_id IS NOT NULL
  AND pending_target_headword IS NOT NULL;

-- 编辑器投影 JSONB 是 GET/词形步保存/跨词条 reconciliation 的权威读源，必须与
-- 关系表同步收窄：旧宽形态的词面挪到只读 target_headword 回显位，否则存量草稿
-- 读路径被新 response 契约拒收、重插关系行时撞新 shape check。
UPDATE lexicon.entry_editor_projection projection
SET meanings = jsonb_set(projection.meanings, '{pos}', (
    SELECT COALESCE(jsonb_agg(
        jsonb_set(pos_item.value, '{senses}', (
            SELECT COALESCE(jsonb_agg(
                jsonb_set(sense_item.value, '{relations}', (
                    SELECT COALESCE(jsonb_agg(
                        CASE
                            WHEN relation_item.value ? 'prebound_target_word_id'
                                 AND relation_item.value ? 'pending_target_headword'
                            THEN (relation_item.value - 'pending_target_headword')
                                 || jsonb_build_object(
                                        'target_headword',
                                        COALESCE(
                                            relation_item.value -> 'target_headword',
                                            relation_item.value -> 'pending_target_headword'
                                        )
                                    )
                            ELSE relation_item.value
                        END
                        ORDER BY relation_item.ordinality
                    ), '[]'::jsonb)
                    FROM jsonb_array_elements(sense_item.value -> 'relations')
                         WITH ORDINALITY AS relation_item
                ))
                ORDER BY sense_item.ordinality
            ), '[]'::jsonb)
            FROM jsonb_array_elements(pos_item.value -> 'senses')
                 WITH ORDINALITY AS sense_item
        ))
        ORDER BY pos_item.ordinality
    ), '[]'::jsonb)
    FROM jsonb_array_elements(projection.meanings -> 'pos')
         WITH ORDINALITY AS pos_item
))
WHERE jsonb_path_exists(
    projection.meanings,
    '$.pos[*].senses[*].relations[*] ? (@.prebound_target_word_id != null && @.pending_target_headword != null)'
);

ALTER TABLE lexicon.relations
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
            AND pending_target_headword IS NULL
            AND (
                pending_target_gloss IS NULL
                OR (
                    pending_target_gloss = btrim(pending_target_gloss)
                    AND char_length(pending_target_gloss) BETWEEN 1 AND 5000
                )
            )
        )
    );
