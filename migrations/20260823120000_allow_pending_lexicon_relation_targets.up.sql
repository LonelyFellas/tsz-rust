-- 关联词在草稿层可以指向尚未建条的词：target 两列可空，待建词面由
-- pending_target_headword 承载；发布时物化成真实词条再回填 target。
--
-- lexicon_relations_target_fkey 保持不动。复合外键在 MATCH SIMPLE（默认）下
-- 遇到 NULL 列不做校验，所以已绑定的关联词照旧受完整约束，只有待物化的那些
-- 带 NULL —— 已发布数据的引用完整性一点没放松。
--
-- target_headword_snapshot / target_gloss_snapshot 改可空：待物化的关联词没有
-- 目标义项，也就没有可快照的内容。两组字段各自只表达一件事，不做语义复用。
ALTER TABLE lexicon.relations
    ALTER COLUMN target_entry_id DROP NOT NULL,
    ALTER COLUMN target_sense_id DROP NOT NULL,
    ALTER COLUMN target_headword_snapshot DROP NOT NULL,
    ALTER COLUMN target_gloss_snapshot DROP NOT NULL,
    ADD COLUMN pending_target_headword TEXT;

ALTER TABLE lexicon.relations
    ADD CONSTRAINT lexicon_relations_target_shape_check CHECK (
        (
            -- 已绑定：指向真实义项，带快照，没有待建词面
            target_entry_id IS NOT NULL
            AND target_sense_id IS NOT NULL
            AND target_headword_snapshot IS NOT NULL
            AND target_gloss_snapshot IS NOT NULL
            AND pending_target_headword IS NULL
        )
        OR (
            -- 待物化：只有待建词面，发布时才会长出目标
            target_entry_id IS NULL
            AND target_sense_id IS NULL
            AND target_headword_snapshot IS NULL
            AND target_gloss_snapshot IS NULL
            AND pending_target_headword IS NOT NULL
            AND pending_target_headword = btrim(pending_target_headword)
            AND char_length(pending_target_headword) BETWEEN 1 AND 200
        )
    );
