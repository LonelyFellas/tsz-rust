-- 待物化的关联词只存在于草稿，且回退后没有列可以承载它们，只能丢弃。
-- 已绑定的关联词不受影响。
DELETE FROM lexicon.relations WHERE target_entry_id IS NULL;

ALTER TABLE lexicon.relations
    DROP CONSTRAINT lexicon_relations_target_shape_check;

ALTER TABLE lexicon.relations
    DROP COLUMN pending_target_headword,
    ALTER COLUMN target_gloss_snapshot SET NOT NULL,
    ALTER COLUMN target_headword_snapshot SET NOT NULL,
    ALTER COLUMN target_sense_id SET NOT NULL,
    ALTER COLUMN target_entry_id SET NOT NULL;
