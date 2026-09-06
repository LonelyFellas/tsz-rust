-- 基本词性仍挂有细分词性时不允许删除（服务层已预检返回 409 part_of_speech_has_sub_parts）；
-- 外键从 CASCADE 改为 RESTRICT，让绕过服务层的直接 SQL 也不会静默连带删掉整组细分词性。
-- 约束名是数据库错误映射契约，不能修改。
ALTER TABLE catalog.sub_parts_of_speech
    DROP CONSTRAINT catalog_sub_parts_parent_fkey,
    ADD CONSTRAINT catalog_sub_parts_parent_fkey
        FOREIGN KEY (part_of_speech_id)
        REFERENCES catalog.parts_of_speech(id)
        ON DELETE RESTRICT;
