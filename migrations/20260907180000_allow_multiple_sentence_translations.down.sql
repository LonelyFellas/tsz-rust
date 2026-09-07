-- 不删除重复译文，也不猜测已退休节点原先的档位。
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM lexicon.text_variants
        GROUP BY owner_node_id, field_role, language, dialect
        HAVING count(*) > 1
    ) OR EXISTS (
        SELECT 1 FROM lexicon.nodes AS node
        WHERE node.node_role = 'meanings.zh_translation'
          AND NOT EXISTS (SELECT 1 FROM lexicon.text_variants AS text WHERE text.id = node.id)
    ) THEN
        RAISE EXCEPTION 'cannot restore translation slots with repeated bands or retired translation nodes'
            USING ERRCODE = '0A000';
    END IF;
END
$$;

UPDATE lexicon.nodes AS node
SET node_role = 'meanings.' || text.field_role || ':zh:common', stable_slot = TRUE
FROM lexicon.text_variants AS text
WHERE node.id = text.id AND node.node_role = 'meanings.zh_translation';

DROP INDEX lexicon.lexicon_text_variants_slot_key;
ALTER TABLE lexicon.text_variants
    ADD CONSTRAINT lexicon_text_variants_slot_key
    UNIQUE (owner_node_id, field_role, language, dialect);
