-- 回退：英美配置收回到词性。专用组、词义绑定、同一词性下各组配置不一致都无法无损折回
-- 词性一级，拒绝回退。编辑器投影 JSON 不在这里改写：部署回退由
-- deployment_migrations::undo 的载荷守卫先行拦截新形状。
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.v3_form_groups WHERE scope = 'dedicated')
        OR EXISTS (SELECT 1 FROM lexicon.senses WHERE form_group_id IS NOT NULL)
        OR EXISTS (
            SELECT 1
            FROM lexicon.v3_form_groups
            GROUP BY entry_pos_id
            HAVING count(DISTINCT spelling_mode || '/' || phonetic_mode) > 1
        )
    THEN
        RAISE EXCEPTION 'cannot fold form group dialect rules back into parts of speech while dedicated groups, sense bindings, or per-group rule differences exist';
    END IF;
END
$$;

ALTER TABLE lexicon.entry_pos
    ADD COLUMN spelling_mode TEXT
        CONSTRAINT lexicon_entry_pos_spelling_mode_check
        CHECK (spelling_mode IN ('unified', 'distinguish')),
    ADD COLUMN phonetic_mode TEXT
        CONSTRAINT lexicon_entry_pos_phonetic_mode_check
        CHECK (phonetic_mode IN ('unified', 'distinguish'));

UPDATE lexicon.entry_pos AS pos
SET spelling_mode = COALESCE((
        SELECT form_group.spelling_mode
        FROM lexicon.v3_form_groups AS form_group
        WHERE form_group.entry_pos_id = pos.id
        ORDER BY form_group.ordinal
        LIMIT 1
    ), 'unified'),
    phonetic_mode = COALESCE((
        SELECT form_group.phonetic_mode
        FROM lexicon.v3_form_groups AS form_group
        WHERE form_group.entry_pos_id = pos.id
        ORDER BY form_group.ordinal
        LIMIT 1
    ), 'unified');

ALTER TABLE lexicon.entry_pos
    ADD CONSTRAINT lexicon_entry_pos_versioned_modes_check CHECK (
        spelling_mode IS NOT NULL
        AND phonetic_mode IS NOT NULL
        AND (spelling_mode <> 'distinguish' OR phonetic_mode = 'distinguish')
    );

ALTER TABLE lexicon.senses
    DROP CONSTRAINT lexicon_senses_form_group_fkey,
    DROP COLUMN form_group_id;

ALTER TABLE lexicon.v3_group_memberships
    DROP CONSTRAINT lexicon_v3_group_memberships_form_key,
    ADD CONSTRAINT lexicon_v3_group_memberships_group_form_key UNIQUE (form_group_id, form_id);

ALTER TABLE lexicon.v3_form_groups
    DROP CONSTRAINT lexicon_v3_form_groups_modes_check,
    DROP COLUMN scope,
    DROP COLUMN spelling_mode,
    DROP COLUMN phonetic_mode;
