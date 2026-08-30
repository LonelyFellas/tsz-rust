-- 多档数据无法无损折回单值。只有每句至多一档时才允许本地回退；正式回滚应关闭
-- 前端能力并保留 additive schema。
DO $$
BEGIN
    IF EXISTS (
        SELECT owner_node_id
        FROM lexicon.text_variants
        WHERE field_role LIKE 'zh_translation_%'
        GROUP BY owner_node_id
        HAVING count(*) > 1
    ) THEN
        RAISE EXCEPTION 'cannot collapse multiple V3 sentence translations into zh_text'
            USING ERRCODE = '0A000';
    END IF;
END
$$;

UPDATE lexicon.text_variants
SET field_role = 'zh_text'
WHERE field_role LIKE 'zh_translation_%';

UPDATE lexicon.nodes AS node
SET node_role = 'meanings.zh_text:zh:common'
FROM lexicon.text_variants AS text
WHERE node.id = text.id
  AND node.entry_id = text.entry_id
  AND text.field_role = 'zh_text'
  AND text.language = 'zh';

ALTER TABLE lexicon.text_variants
    DROP CONSTRAINT lexicon_text_variants_field_role_check;

ALTER TABLE lexicon.text_variants
    ADD CONSTRAINT lexicon_text_variants_field_role_check
        CHECK (field_role IN ('content', 'en_text', 'zh_text'));
