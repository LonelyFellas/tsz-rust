-- TASK#45：英美配置从词性下沉到变化组；变化组分通用 / 专用；词义可绑定本词性的专用组。
-- 不兼容存量（见 docs/features/form-group-dialect-scope）：组上的新列 NOT NULL 且不给默认值，
-- 旧投影 JSON 也不回填。测试服按口径先清库再部署，有 V3 词条时直接拒绝。
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon.entries
        WHERE content_schema_version = 3
    ) THEN
        RAISE EXCEPTION 'legacy V3 data is unsupported; reset Smart Lexicon before applying form group dialect scope';
    END IF;
END
$$;

ALTER TABLE lexicon.v3_form_groups
    ADD COLUMN scope TEXT NOT NULL
        CONSTRAINT lexicon_v3_form_groups_scope_check CHECK (scope IN ('general', 'dedicated')),
    ADD COLUMN spelling_mode TEXT NOT NULL
        CONSTRAINT lexicon_v3_form_groups_spelling_mode_check
        CHECK (spelling_mode IN ('unified', 'distinguish')),
    ADD COLUMN phonetic_mode TEXT NOT NULL
        CONSTRAINT lexicon_v3_form_groups_phonetic_mode_check
        CHECK (phonetic_mode IN ('unified', 'distinguish')),
    ADD CONSTRAINT lexicon_v3_form_groups_modes_check
        CHECK (spelling_mode <> 'distinguish' OR phonetic_mode = 'distinguish');

-- 一形一组：新唯一键蕴含旧的 (form_group_id, form_id)。
ALTER TABLE lexicon.v3_group_memberships
    DROP CONSTRAINT lexicon_v3_group_memberships_group_form_key,
    ADD CONSTRAINT lexicon_v3_group_memberships_form_key UNIQUE (form_id);

-- 复合外键顺带锁死「绑定的组与词义同属一个词性」。
-- 词形保存在同一事务里先重写词义、再整体删掉并重插全部组，所以：
--   * 不能用 ON DELETE CASCADE（删组那一刻会连带删掉刚写入的词义）；
--   * 检查必须延迟到提交（重插同 id 的组后外键自然成立）。
-- 真正被删掉的组，reconcile 已先清掉词义上的绑定；漏清则提交时失败，不会留下悬空引用。
ALTER TABLE lexicon.senses
    ADD COLUMN form_group_id UUID,
    ADD CONSTRAINT lexicon_senses_form_group_fkey
        FOREIGN KEY (form_group_id, entry_pos_id, entry_id)
        REFERENCES lexicon.v3_form_groups(id, entry_pos_id, entry_id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE lexicon.entry_pos
    DROP CONSTRAINT lexicon_entry_pos_versioned_modes_check,
    DROP COLUMN spelling_mode,
    DROP COLUMN phonetic_mode;
