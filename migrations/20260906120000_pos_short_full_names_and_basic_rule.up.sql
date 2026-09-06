-- 基本词性目录对齐产品原型 + "基础词性"规则（tsz docs/features/pos-catalog-prototype-alignment）：
--   1. 新增 short_name_zh（简洁显示）、full_name_en（英文全称）两个必填列；
--   2. 细分词性只允许挂在五个基础词性（noun/verb/pronoun/adjective/adverb）下；
--   3. 六个非基础种子基本词性（介词/冠词/限定词/连词/数词/感叹词）连同其细分词性下线；
--   4. 种子按固定 UUID 幂等重建，空目录（本地已清空）与已有种子（测试服）都能跑。
-- 被词条引用的非基础词性/细分词性会被外键 RESTRICT 拦下使迁移失败——这是预期行为，
-- 需先清理词条数据（lexicon.*）再重跑，不做静默跳过。

ALTER TABLE catalog.parts_of_speech
    ADD COLUMN short_name_zh TEXT,
    ADD COLUMN full_name_en TEXT;

-- 规则：非基础词性下的细分词性全部下线（含管理员自建的）。
DELETE FROM catalog.sub_parts_of_speech AS s
USING catalog.parts_of_speech AS p
WHERE p.id = s.part_of_speech_id
  AND p.code NOT IN ('noun', 'verb', 'pronoun', 'adjective', 'adverb');

-- 六个非基础种子基本词性下线（固定 ID 来自 20260810050408）。
DELETE FROM catalog.parts_of_speech
WHERE id IN (
    '019fea10-20ec-7279-a4dc-634c5c4ffca1', -- preposition
    '019fea10-20ec-79ae-9bdd-83d5c58ff641', -- article
    '019fea10-20ec-7a6f-b827-0c169308ee45', -- determiner
    '019fea10-20ec-7d64-acea-96a48d51c8ed', -- conjunction
    '019fea10-20ec-76fc-a8c8-20b1e2cd1da9', -- numeral
    '019fea10-20ec-7554-b9ac-f4485b6d7463'  -- interjection
);

-- 五个基础词性种子：空表时种回；已存在时只补两个新列，不覆盖管理员可能改过的展示字段。
INSERT INTO catalog.parts_of_speech (
    id, code, name_zh, name_en, abbreviation, short_name_zh, full_name_en, sort_order
)
VALUES
    ('019fea10-20ec-7154-bb63-0b37991c0c68', 'noun',      '名词',   'NOUN',      'n.',    '名词',   'noun',      10),
    ('019fea10-20ec-7c51-9f71-86327e7c934e', 'pronoun',   '代词',   'PRONOUN',   'pron.', '代词',   'pronoun',   20),
    ('019fea10-20ec-7f41-a252-70261233fa51', 'verb',      '动词',   'VERB',      'v.',    '动词',   'verb',      30),
    ('019fea10-20ec-79f4-98c5-6c45736a73bb', 'adjective', '形容词', 'ADJECTIVE', 'adj.',  '形容词', 'adjective', 40),
    ('019fea10-20ec-72ed-b6ae-d243bd77a333', 'adverb',    '副词',   'ADVERB',    'adv.',  '副词',   'adverb',    50)
ON CONFLICT (id) DO UPDATE
SET short_name_zh = EXCLUDED.short_name_zh,
    full_name_en = EXCLUDED.full_name_en;

-- 五个基础词性下的 13 个细分词性种子（固定 ID 来自 20260810080727）。
INSERT INTO catalog.sub_parts_of_speech (
    id, part_of_speech_id, code, name_zh, name_en, sort_order
)
VALUES
    ('019feab6-ca12-7cf6-bfe9-500e066ee6ae', '019fea10-20ec-7f41-a252-70261233fa51', 'V-T',       '及物动词',   'Transitive verb',   10),
    ('019feab6-ca13-7d94-8779-bc9355744065', '019fea10-20ec-7f41-a252-70261233fa51', 'V-I',       '不及物动词', 'Intransitive verb', 20),
    ('019feab6-ca13-7455-b5b1-67f348988d16', '019fea10-20ec-7f41-a252-70261233fa51', 'V-LINK',    '系动词',     'Linking verb',      30),
    ('019feab6-ca13-76a4-8716-d25dbba54d8c', '019fea10-20ec-7f41-a252-70261233fa51', 'AUX',       '助动词',     'Auxiliary verb',    40),
    ('019feab6-ca13-7396-8895-8de4acfb83c6', '019fea10-20ec-7f41-a252-70261233fa51', 'MODAL',     '情态动词',   'Modal verb',        50),
    ('019feab6-ca13-7738-a213-5cb1f9c74383', '019fea10-20ec-79f4-98c5-6c45736a73bb', 'ADJ',       '形容词',     'Adjective',         60),
    ('019feab6-ca13-7af3-9578-0157650ea3d0', '019fea10-20ec-72ed-b6ae-d243bd77a333', 'ADV',       '副词',       'Adverb',            70),
    ('019feab6-ca13-75ef-ab3f-729125ebcede', '019fea10-20ec-7154-bb63-0b37991c0c68', 'N-COUNT',   '可数名词',   'Countable noun',    80),
    ('019feab6-ca13-7a20-aca2-d386e0ed3961', '019fea10-20ec-7154-bb63-0b37991c0c68', 'N-UNCOUNT', '不可数名词', 'Uncountable noun',  90),
    ('019feab6-ca13-72ec-86f3-619d1577b2da', '019fea10-20ec-7154-bb63-0b37991c0c68', 'N-PROPER',  '专有名词',   'Proper noun',      100),
    ('019feab6-ca13-7a7b-8cc5-4dafd4b1b3f8', '019fea10-20ec-7154-bb63-0b37991c0c68', 'N-PLURAL',  '复数名词',   'Plural noun',      110),
    ('019feab6-ca13-7ac1-b09e-2edf6d38b086', '019fea10-20ec-7154-bb63-0b37991c0c68', 'N-SING',    '单数名词',   'Singular noun',    120),
    ('019feab6-ca13-7bf9-9317-4f7b50afcddd', '019fea10-20ec-7c51-9f71-86327e7c934e', 'PRON',      '代词',       'Pronoun',          130)
ON CONFLICT (id) DO NOTHING;

-- 管理员自建的存量基本词性回填：简洁显示取正式中文，英文全称取正式英文小写。
-- 回填若撞唯一索引或长度 CHECK，迁移直接失败并暴露冲突行，人工处理后重跑。
UPDATE catalog.parts_of_speech
SET short_name_zh = COALESCE(short_name_zh, name_zh),
    full_name_en = COALESCE(full_name_en, lower(name_en));

ALTER TABLE catalog.parts_of_speech
    ALTER COLUMN short_name_zh SET NOT NULL,
    ALTER COLUMN full_name_en SET NOT NULL,
    ADD CONSTRAINT catalog_parts_of_speech_short_name_zh_check
        CHECK (
            short_name_zh = btrim(short_name_zh)
            AND char_length(short_name_zh) BETWEEN 1 AND 16
        ),
    ADD CONSTRAINT catalog_parts_of_speech_full_name_en_check
        CHECK (
            full_name_en = btrim(full_name_en)
            AND char_length(full_name_en) BETWEEN 1 AND 64
        );

-- 这些索引名是数据库错误映射契约，不能修改。
CREATE UNIQUE INDEX catalog_parts_of_speech_short_name_zh_unique_idx
    ON catalog.parts_of_speech (short_name_zh);

CREATE UNIQUE INDEX catalog_parts_of_speech_full_name_en_unique_idx
    ON catalog.parts_of_speech (lower(full_name_en));

-- 目录版本递增，前端缓存据此失效。
UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
