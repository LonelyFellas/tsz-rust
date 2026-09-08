-- 不删除重复译文，也不猜测迁移后新建节点原先不存在的稳定槽位。
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM lexicon.text_variants
        GROUP BY owner_node_id, field_role, language, dialect
        HAVING count(*) > 1
    ) OR EXISTS (
        SELECT 1 FROM lexicon.nodes AS node
        WHERE node.node_role = 'meanings.zh_translation'
          AND NOT EXISTS (
              SELECT 1
              FROM lexicon.sentence_translation_slot_rollback_v20260907180000 AS rollback
              WHERE rollback.node_id = node.id
          )
    ) OR EXISTS (
        SELECT 1
        FROM lexicon.sentence_translation_slot_rollback_v20260907180000 AS rollback
        JOIN lexicon.text_variants AS text ON text.id = rollback.node_id
        WHERE rollback.node_role <> 'meanings.' || text.field_role || ':'
            || text.language || ':' || text.dialect
    ) THEN
        RAISE EXCEPTION 'cannot restore translation slots after repeated, new, or rebanded translations'
            USING ERRCODE = '0A000';
    END IF;
END
$$;

UPDATE lexicon.nodes AS node
SET node_role = rollback.node_role, stable_slot = rollback.stable_slot
FROM lexicon.sentence_translation_slot_rollback_v20260907180000 AS rollback
WHERE node.id = rollback.node_id
  AND node.node_role = 'meanings.zh_translation';

DROP INDEX lexicon.lexicon_text_variants_slot_key;
ALTER TABLE lexicon.text_variants
    ADD CONSTRAINT lexicon_text_variants_slot_key
    UNIQUE (owner_node_id, field_role, language, dialect);

DROP TABLE lexicon.sentence_translation_slot_rollback_v20260907180000;
