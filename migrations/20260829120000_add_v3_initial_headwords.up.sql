-- Step 1 的最终主词是管理员确认值，不再等同于检测建议。
-- native V3 不写 legacy entry_headwords；把创建时确认值保存在 V3 自有状态中。
ALTER TABLE lexicon.v3_entry_state
    ADD COLUMN initial_headwords JSONB;

ALTER TABLE lexicon.v3_entry_state
    ADD CONSTRAINT lexicon_v3_entry_state_initial_headwords_shape_check CHECK (
        initial_headwords IS NULL
        OR (
            initial_headwords ->> 'mode' = 'unified'
            AND jsonb_typeof(initial_headwords -> 'common') = 'string'
            AND btrim(initial_headwords ->> 'common') <> ''
            AND NOT initial_headwords ?| ARRAY['uk', 'us', 'source_dialect']
        )
        OR (
            initial_headwords ->> 'mode' = 'distinguish'
            AND jsonb_typeof(initial_headwords -> 'uk') = 'string'
            AND jsonb_typeof(initial_headwords -> 'us') = 'string'
            AND btrim(initial_headwords ->> 'uk') <> ''
            AND btrim(initial_headwords ->> 'us') <> ''
            AND initial_headwords ->> 'source_dialect' IN ('uk', 'us')
            AND NOT initial_headwords ? 'common'
        )
    );
