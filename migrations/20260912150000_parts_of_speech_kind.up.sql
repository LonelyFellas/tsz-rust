-- 基本词性加 word / phrase 维度（禅道 TASK#36，spec 见 docs/features/phrase-parts-of-speech）：
--   1. parts_of_speech 加 kind 列，存量全部为 word；短语词性的 code 必须以 phrase_ 开头；
--   2. 五个展示字段的唯一索引从全局收敛到同一 kind 内（短语侧可再建「名词」），code 仍全局唯一；
--   3. entry_pos 记下词条 kind，用复合外键锁死「单词词条只能挂单词词性、短语词条只能挂短语词性」；
--   4. form_types 只能挂 word 词性。
-- 索引名与外键名是数据库错误映射契约，重建时原样保留。

-- 1. kind 列：存量全部为 word；默认值保留，直接写 SQL 的地方（种子、测试）不给就是单词词性。
ALTER TABLE catalog.parts_of_speech
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'word'
        CONSTRAINT catalog_parts_of_speech_kind_check CHECK (kind IN ('word', 'phrase'));
ALTER TABLE catalog.parts_of_speech
    -- V3 词条、发布快照与细分词性父级映射都按 code 引用词性，code 不能按 kind 重名；
    -- 短语侧用前缀隔出自己的命名空间，管理员不填不看 code，由前端派生时加前缀。
    ADD CONSTRAINT catalog_parts_of_speech_phrase_code_check
        CHECK (kind <> 'phrase' OR code LIKE 'phrase\_%'),
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

-- 存量短语词条在本次之前只能挂单词词性：不清数据，外键用 NOT VALID 豁免迁移时已有的行，
-- 之后的新写入照常检查，删词性的 RESTRICT 也照常生效。这些词条下次编辑词形或发布时会被
-- part_of_speech_kind_mismatch 校验拦下，管理员改选短语词性即可。
ALTER TABLE lexicon.entry_pos
    ALTER COLUMN entry_kind SET NOT NULL,
    ADD CONSTRAINT lexicon_entry_pos_entry_kind_check CHECK (entry_kind IN ('word', 'phrase')),
    ADD CONSTRAINT lexicon_entry_pos_entry_kind_fkey
        FOREIGN KEY (entry_id, entry_kind)
        REFERENCES lexicon.entries(id, kind)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    DROP CONSTRAINT lexicon_entry_pos_catalog_pos_fkey,
    ADD CONSTRAINT lexicon_entry_pos_catalog_pos_fkey
        FOREIGN KEY (part_of_speech_id, entry_kind)
        REFERENCES catalog.parts_of_speech(id, kind)
        ON DELETE RESTRICT
        NOT VALID;

DO $$
DECLARE
    legacy_count BIGINT;
BEGIN
    SELECT count(*)
    INTO legacy_count
    FROM lexicon.entry_pos pos
    JOIN catalog.parts_of_speech p ON p.id = pos.part_of_speech_id
    WHERE pos.entry_kind <> p.kind;
    IF legacy_count > 0 THEN
        RAISE NOTICE '% entry_pos rows reference a part of speech of another kind; they stay until the entry is edited', legacy_count;
    END IF;
END
$$;

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
