-- 基本词性加 word / phrase 维度（禅道 TASK#36，spec 见 docs/features/phrase-parts-of-speech）：
--   1. parts_of_speech 加 kind 列，存量全部为 word；短语词性的 code 必须以 phrase_ 开头；
--   2. 五个展示字段的唯一索引从全局收敛到同一 kind 内（短语侧可再建「名词」），code 仍全局唯一；
--   3. entry_pos 记下词条 kind，另起一条复合外键锁死「单词词条只能挂单词词性、短语词条只能挂短语词性」，
--      原有的单列引用外键保持不动（它承担 in-use 保护，必须无条件覆盖存量行）；
--   4. form_types 只能挂 word 词性。
-- 索引名与外键名是数据库错误映射契约，重建时原样保留。

-- 存量若有管理员自建、code 已占用 phrase_ 前缀的单词词性，下面的双向 CHECK 会失败。
-- 先报出具体 code 让人改名，而不是抛一条看不懂的 23514。
DO $$
DECLARE
    squatters TEXT;
BEGIN
    SELECT string_agg(code, ', ' ORDER BY code)
    INTO squatters
    FROM catalog.parts_of_speech
    WHERE code LIKE 'phrase\_%';
    IF squatters IS NOT NULL THEN
        RAISE EXCEPTION
            'existing parts of speech already occupy the phrase_ code namespace: %',
            squatters;
    END IF;
END
$$;

-- 1. kind 列：存量全部为 word；默认值保留，直接写 SQL 的地方（种子、测试）不给就是单词词性。
ALTER TABLE catalog.parts_of_speech
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'word'
        CONSTRAINT catalog_parts_of_speech_kind_check CHECK (kind IN ('word', 'phrase'));
ALTER TABLE catalog.parts_of_speech
    -- V3 词条、发布快照与细分词性父级映射都按 code 引用词性，code 不能按 kind 重名；
    -- 短语侧用前缀隔出自己的命名空间。双向等价：短语必须带前缀，单词不许占用该前缀，
    -- 否则单词侧可以把 phrase_xxx 占掉，短语侧再建同名只会撞到看不懂的 code 冲突。
    -- 管理员不填不看 code，由前端派生时加前缀。
    ADD CONSTRAINT catalog_parts_of_speech_phrase_code_check
        CHECK ((code LIKE 'phrase\_%') = (kind = 'phrase')),
    ADD CONSTRAINT catalog_parts_of_speech_id_kind_key UNIQUE (id, kind);

-- 2. 展示字段唯一性收敛到 kind 内。
DROP INDEX catalog.catalog_parts_of_speech_name_zh_unique_idx;
DROP INDEX catalog.catalog_parts_of_speech_name_en_unique_idx;
DROP INDEX catalog.catalog_parts_of_speech_abbreviation_unique_idx;
DROP INDEX catalog.catalog_parts_of_speech_short_name_zh_unique_idx;
DROP INDEX catalog.catalog_parts_of_speech_full_name_en_unique_idx;
CREATE UNIQUE INDEX catalog_parts_of_speech_name_zh_unique_idx
    ON catalog.parts_of_speech (kind, name_zh);
CREATE UNIQUE INDEX catalog_parts_of_speech_name_en_unique_idx
    ON catalog.parts_of_speech (kind, lower(name_en));
CREATE UNIQUE INDEX catalog_parts_of_speech_abbreviation_unique_idx
    ON catalog.parts_of_speech (kind, lower(abbreviation));
CREATE UNIQUE INDEX catalog_parts_of_speech_short_name_zh_unique_idx
    ON catalog.parts_of_speech (kind, short_name_zh);
CREATE UNIQUE INDEX catalog_parts_of_speech_full_name_en_unique_idx
    ON catalog.parts_of_speech (kind, lower(full_name_en));

-- 3. 词条侧：entries 给出 (id, kind) 二元组，entry_pos 记下词条 kind 并用两条复合外键锁死。
ALTER TABLE lexicon.entries
    ADD CONSTRAINT lexicon_entries_id_kind_key UNIQUE (id, kind);

ALTER TABLE lexicon.entry_pos ADD COLUMN entry_kind TEXT;
UPDATE lexicon.entry_pos AS pos
SET entry_kind = e.kind
FROM lexicon.entries AS e
WHERE e.id = pos.entry_id;

-- 存量短语词条在本次之前只能挂单词词性：不清数据，kind 配对外键用 NOT VALID 豁免迁移时已有的行。
-- 这些词条下次编辑词形、完成或发布时会被 part_of_speech_kind_mismatch 校验拦下，管理员改选短语词性即可。
--
-- 单列的 lexicon_entry_pos_catalog_pos_fkey **原样保留**，不改成复合外键：
-- 复合外键对存量错配行（(part_id,'phrase') 匹配不到 (part_id,'word')）视同没有引用，
-- 删词性会在数据库层直接放行并留下悬空引用，也让该约束名到 part_of_speech_in_use 的错误映射失效。
-- 引用保护由单列外键无条件承担，kind 配对交给另一条独立的新约束。
ALTER TABLE lexicon.entry_pos
    ALTER COLUMN entry_kind SET NOT NULL,
    ADD CONSTRAINT lexicon_entry_pos_entry_kind_check CHECK (entry_kind IN ('word', 'phrase')),
    ADD CONSTRAINT lexicon_entry_pos_entry_kind_fkey
        FOREIGN KEY (entry_id, entry_kind)
        REFERENCES lexicon.entries(id, kind)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT lexicon_entry_pos_catalog_kind_fkey
        FOREIGN KEY (part_of_speech_id, entry_kind)
        REFERENCES catalog.parts_of_speech(id, kind)
        ON DELETE RESTRICT
        NOT VALID;

-- 4. 词形变化只认 word 词性：恒为 word 的普通列 + 复合外键，不写触发器。
-- 原形的 part_of_speech_id 为 NULL，MATCH SIMPLE 下不参与外键检查。
ALTER TABLE catalog.form_types
    ADD COLUMN part_of_speech_kind TEXT NOT NULL DEFAULT 'word'
        CONSTRAINT catalog_form_types_part_of_speech_kind_check CHECK (part_of_speech_kind = 'word'),
    DROP CONSTRAINT catalog_form_types_part_of_speech_fkey,
    ADD CONSTRAINT catalog_form_types_part_of_speech_fkey
        FOREIGN KEY (part_of_speech_id, part_of_speech_kind)
        REFERENCES catalog.parts_of_speech(id, kind)
        ON DELETE RESTRICT;

-- 目录响应形状变了（每项多 kind），版本递增让前端缓存失效。
UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
