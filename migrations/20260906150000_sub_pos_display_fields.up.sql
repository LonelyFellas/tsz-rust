-- 细分词性对齐产品原型（tsz docs/features/pos-catalog-prototype-alignment）：
-- 新增 short_name_zh（简洁显示）、abbreviation（英文缩写）、full_name_en（英文全称）三个必填列。
-- 简洁显示与缩写允许在同一父级下重复（原型里多个名词短语共用"n."），英文全称在同一父级下忽略大小写唯一。

ALTER TABLE catalog.sub_parts_of_speech
    ADD COLUMN short_name_zh TEXT,
    ADD COLUMN abbreviation TEXT,
    ADD COLUMN full_name_en TEXT;

-- 13 个基础细分词性种子按固定 ID 补齐三列（ID 来自 20260810080727）。
UPDATE catalog.sub_parts_of_speech AS s
SET short_name_zh = v.short_name_zh,
    abbreviation = v.abbreviation,
    full_name_en = v.full_name_en
FROM (
    VALUES
        ('019feab6-ca12-7cf6-bfe9-500e066ee6ae'::uuid, '及物动词',   'vt.',      'transitive verb'),
        ('019feab6-ca13-7d94-8779-bc9355744065'::uuid, '不及物动词', 'vi.',      'intransitive verb'),
        ('019feab6-ca13-7455-b5b1-67f348988d16'::uuid, '系动词',     'link.v.',  'linking verb'),
        ('019feab6-ca13-76a4-8716-d25dbba54d8c'::uuid, '助动词',     'aux.',     'auxiliary verb'),
        ('019feab6-ca13-7396-8895-8de4acfb83c6'::uuid, '情态动词',   'modal v.', 'modal verb'),
        ('019feab6-ca13-7738-a213-5cb1f9c74383'::uuid, '形容词',     'adj.',     'adjective'),
        ('019feab6-ca13-7af3-9578-0157650ea3d0'::uuid, '副词',       'adv.',     'adverb'),
        ('019feab6-ca13-75ef-ab3f-729125ebcede'::uuid, '可数名词',   'n.',       'countable noun'),
        ('019feab6-ca13-7a20-aca2-d386e0ed3961'::uuid, '不可数名词', 'n.',       'uncountable noun'),
        ('019feab6-ca13-72ec-86f3-619d1577b2da'::uuid, '专有名词',   'n.',       'proper noun'),
        ('019feab6-ca13-7a7b-8cc5-4dafd4b1b3f8'::uuid, '复数名词',   'n-pl.',    'plural noun'),
        ('019feab6-ca13-7ac1-b09e-2edf6d38b086'::uuid, '单数名词',   'n.',       'singular noun'),
        ('019feab6-ca13-7bf9-9317-4f7b50afcddd'::uuid, '代词',       'pron.',    'pronoun')
) AS v(id, short_name_zh, abbreviation, full_name_en)
WHERE s.id = v.id;

-- 存量自建行回填：简洁显示取正式中文，缩写沿用所属基本词性的缩写，英文全称取正式英文小写。
UPDATE catalog.sub_parts_of_speech AS s
SET short_name_zh = COALESCE(s.short_name_zh, s.name_zh),
    abbreviation = COALESCE(s.abbreviation, p.abbreviation),
    full_name_en = COALESCE(s.full_name_en, lower(s.name_en))
FROM catalog.parts_of_speech AS p
WHERE p.id = s.part_of_speech_id;

ALTER TABLE catalog.sub_parts_of_speech
    ALTER COLUMN short_name_zh SET NOT NULL,
    ALTER COLUMN abbreviation SET NOT NULL,
    ALTER COLUMN full_name_en SET NOT NULL,
    ADD CONSTRAINT catalog_sub_parts_short_name_zh_check
        CHECK (
            short_name_zh = btrim(short_name_zh)
            AND char_length(short_name_zh) BETWEEN 1 AND 16
        ),
    ADD CONSTRAINT catalog_sub_parts_abbreviation_check
        CHECK (
            abbreviation = btrim(abbreviation)
            AND char_length(abbreviation) BETWEEN 1 AND 16
        ),
    ADD CONSTRAINT catalog_sub_parts_full_name_en_check
        CHECK (
            full_name_en = btrim(full_name_en)
            AND char_length(full_name_en) BETWEEN 1 AND 64
        );

-- 索引名是数据库错误映射契约，不能修改。
CREATE UNIQUE INDEX catalog_sub_parts_full_name_en_unique_idx
    ON catalog.sub_parts_of_speech (part_of_speech_id, lower(full_name_en));

UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
