-- 分档译文由稳定 ID 标识，同句同档可以有多条。其他正文槽位继续唯一。
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
