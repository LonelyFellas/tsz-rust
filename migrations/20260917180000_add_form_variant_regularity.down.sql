-- 旧版严格 DTO 不认识变体 is_regular，不能在已有新格式数据时直接退回。
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM lexicon.entry_editor_projection
        WHERE jsonb_path_exists(forms, '$.pos[*].forms[*].regional_variants.*.is_regular')
    ) OR EXISTS (
        SELECT 1 FROM lexicon.entry_publications
        WHERE content_schema_version = 3
          AND jsonb_path_exists(snapshot, '$.forms.pos[*].forms[*].regional_variants.*.is_regular')
    ) THEN
        RAISE EXCEPTION 'cannot rollback spelling regularity while variant is_regular exists in drafts or publications';
    END IF;
END $$;

ALTER TABLE lexicon.v3_form_variants DROP COLUMN is_regular;
