-- 词形变化从全局目录改为挂在基本词性下：原形仍对所有词性通用，其余各归一个基本词性。
ALTER TABLE catalog.form_types
    ADD COLUMN part_of_speech_id UUID REFERENCES catalog.parts_of_speech(id) ON DELETE RESTRICT;

UPDATE catalog.form_types f SET part_of_speech_id = p.id
FROM catalog.parts_of_speech p
WHERE p.code = 'verb'
  AND f.code IN ('third_person_singular', 'present_participle', 'past_tense', 'past_participle');

UPDATE catalog.form_types f SET part_of_speech_id = p.id
FROM catalog.parts_of_speech p
WHERE p.code = 'noun' AND f.code = 'plural';

UPDATE catalog.form_types f SET part_of_speech_id = p.id
FROM catalog.parts_of_speech p
WHERE p.code = 'adjective' AND f.code IN ('comparative', 'superlative');

-- 管理员自建的词形无法从数据推断归属，统一落到名词下，之后可在配置页改。
UPDATE catalog.form_types f SET part_of_speech_id = p.id
FROM catalog.parts_of_speech p
WHERE p.code = 'noun' AND f.code <> 'base' AND f.part_of_speech_id IS NULL;

ALTER TABLE catalog.form_types
    ADD CONSTRAINT catalog_form_types_base_is_global
    CHECK ((code = 'base') = (part_of_speech_id IS NULL));

-- 展示字段的唯一性从全局收敛到同一基本词性内：形容词与副词可以各有一个「比较级」。
DROP INDEX catalog.catalog_form_types_name_zh_unique_idx;
DROP INDEX catalog.catalog_form_types_name_en_unique_idx;
DROP INDEX catalog.catalog_form_types_short_name_zh_unique_idx;
DROP INDEX catalog.catalog_form_types_abbreviation_unique_idx;
DROP INDEX catalog.catalog_form_types_full_name_en_unique_idx;
CREATE UNIQUE INDEX catalog_form_types_name_zh_unique_idx
    ON catalog.form_types (part_of_speech_id, name_zh) NULLS NOT DISTINCT;
CREATE UNIQUE INDEX catalog_form_types_name_en_unique_idx
    ON catalog.form_types (part_of_speech_id, lower(name_en)) NULLS NOT DISTINCT;
CREATE UNIQUE INDEX catalog_form_types_short_name_zh_unique_idx
    ON catalog.form_types (part_of_speech_id, short_name_zh) NULLS NOT DISTINCT;
CREATE UNIQUE INDEX catalog_form_types_abbreviation_unique_idx
    ON catalog.form_types (part_of_speech_id, lower(abbreviation)) NULLS NOT DISTINCT;
CREATE UNIQUE INDEX catalog_form_types_full_name_en_unique_idx
    ON catalog.form_types (part_of_speech_id, lower(full_name_en)) NULLS NOT DISTINCT;
CREATE INDEX catalog_form_types_pos_order_idx
    ON catalog.form_types (part_of_speech_id, sort_order, created_at, id);
