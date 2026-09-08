-- 分档译文由稳定 ID 标识，同句同档可以有多条。其他正文槽位继续唯一。
-- 回退时必须恢复已经退休、因而没有 text_variants 行的旧稳定槽位；先保存原身份，
-- 不能在 down migration 里靠当前正文反推。
CREATE TABLE lexicon.sentence_translation_slot_rollback_v20260907180000 (
    node_id UUID PRIMARY KEY,
    node_role TEXT NOT NULL CHECK (node_role IN (
        'meanings.zh_translation_a1_a2:zh:common',
        'meanings.zh_translation_b1_b2:zh:common',
        'meanings.zh_translation_c1_c2:zh:common'
    )),
    stable_slot BOOLEAN NOT NULL
);

INSERT INTO lexicon.sentence_translation_slot_rollback_v20260907180000 (
    node_id, node_role, stable_slot
)
SELECT node.id, node.node_role, node.stable_slot
FROM lexicon.nodes AS node
WHERE node.node_type = 'text_variant'
  AND node.node_role IN (
      'meanings.zh_translation_a1_a2:zh:common',
      'meanings.zh_translation_b1_b2:zh:common',
      'meanings.zh_translation_c1_c2:zh:common'
  );

ALTER TABLE lexicon.text_variants
    DROP CONSTRAINT lexicon_text_variants_slot_key;

CREATE UNIQUE INDEX lexicon_text_variants_slot_key
    ON lexicon.text_variants (owner_node_id, field_role, language, dialect)
    WHERE field_role NOT IN (
        'zh_translation_a1_a2', 'zh_translation_b1_b2', 'zh_translation_c1_c2'
    );

UPDATE lexicon.nodes
SET node_role = 'meanings.zh_translation', stable_slot = FALSE
WHERE node_type = 'text_variant'
  AND node_role IN (
      'meanings.zh_translation_a1_a2:zh:common',
      'meanings.zh_translation_b1_b2:zh:common',
      'meanings.zh_translation_c1_c2:zh:common'
  );
