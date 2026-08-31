-- V3 例句中文译文由单值扩展为最多三档。继续复用 text_variants：field_role
-- 直接编码闭合 band，现有 slot 唯一约束即保证同一句同档最多一条。

ALTER TABLE lexicon.text_variants
    DROP CONSTRAINT lexicon_text_variants_field_role_check;

ALTER TABLE lexicon.text_variants
    ADD CONSTRAINT lexicon_text_variants_field_role_check CHECK (
        field_role IN (
            'content', 'en_text', 'zh_text',
            'zh_translation_a1_a2',
            'zh_translation_b1_b2',
            'zh_translation_c1_c2'
        )
    );

UPDATE lexicon.text_variants AS text
SET field_role = CASE
    WHEN sentence.level IN ('C1', 'C2') THEN 'zh_translation_c1_c2'
    WHEN sentence.level IN ('B1', 'B2') THEN 'zh_translation_b1_b2'
    ELSE 'zh_translation_a1_a2'
END
FROM lexicon.sentences AS sentence
JOIN lexicon.entries AS entry
  ON entry.id = sentence.entry_id
 AND entry.content_schema_version = 3
WHERE text.entry_id = sentence.entry_id
  AND text.owner_node_id = sentence.id
  AND text.field_role = 'zh_text'
  AND text.language = 'zh'
  AND text.dialect = 'common';

UPDATE lexicon.nodes AS node
SET node_role = 'meanings.' || text.field_role || ':zh:common'
FROM lexicon.text_variants AS text
WHERE node.id = text.id
  AND node.entry_id = text.entry_id
  AND text.field_role LIKE 'zh_translation_%';
