-- V3 Step 1 的检测结果是建议，管理员最终确认值需要独立于 detection snapshot 持久化。
-- expand 阶段允许历史行暂时为空；新后端会为所有新 native V3 写入成对字段。
ALTER TABLE lexicon.v3_entry_state
    ADD COLUMN initial_headwords JSONB,
    ADD COLUMN initial_headword_keys TEXT[];

ALTER TABLE lexicon.v3_entry_state
    ADD CONSTRAINT lexicon_v3_entry_state_initial_headwords_shape_check CHECK (
        (
            initial_headwords IS NULL
            AND initial_headword_keys IS NULL
        )
        OR (
            initial_headwords IS NOT NULL
            AND initial_headword_keys IS NOT NULL
            AND jsonb_typeof(initial_headwords) = 'object'
            AND cardinality(initial_headword_keys) = 2
            AND array_position(initial_headword_keys, NULL) IS NULL
            AND initial_headword_keys[1] LIKE 'uk:_%'
            AND initial_headword_keys[2] LIKE 'us:_%'
            AND ((
                (
                    initial_headwords ->> 'mode' = 'unified'
                    AND initial_headwords ?& ARRAY['mode', 'common']
                    AND initial_headwords - ARRAY['mode', 'common'] = '{}'::JSONB
                    AND jsonb_typeof(initial_headwords -> 'common') = 'string'
                    AND btrim(initial_headwords ->> 'common') <> ''
                )
                OR (
                    initial_headwords ->> 'mode' = 'distinguish'
                    AND initial_headwords ?& ARRAY['mode', 'uk', 'us', 'source_dialect']
                    AND initial_headwords - ARRAY['mode', 'uk', 'us', 'source_dialect'] = '{}'::JSONB
                    AND jsonb_typeof(initial_headwords -> 'uk') = 'string'
                    AND jsonb_typeof(initial_headwords -> 'us') = 'string'
                    AND btrim(initial_headwords ->> 'uk') <> ''
                    AND btrim(initial_headwords ->> 'us') <> ''
                    AND initial_headwords ->> 'source_dialect' IN ('uk', 'us')
                )
            ) IS TRUE)
        )
    );
