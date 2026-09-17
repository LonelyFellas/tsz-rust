ALTER TABLE lexicon.v3_form_variants ADD COLUMN is_regular BOOLEAN;

-- 仅回填当前关系投影，历史发布快照保持不可变。
UPDATE lexicon.v3_form_variants AS variant
SET is_regular = COALESCE((
    SELECT bool_and(form_group.is_regular)
    FROM lexicon.v3_group_memberships AS membership
    JOIN lexicon.v3_form_groups AS form_group ON form_group.id = membership.form_group_id
    WHERE membership.form_id = variant.form_id
), TRUE);
