-- Refuse rollback rather than silently changing a reading's meaning.
ALTER TABLE lexicon.v3_pronunciations
    DROP CONSTRAINT lexicon_v3_pronunciations_style_check,
    ADD CONSTRAINT lexicon_v3_pronunciations_style_check
        CHECK (style IN ('normal', 'strong', 'weak'));

-- Old application versions cannot decode citation values in drafts or history either.
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM lexicon.entry_editor_projection
        WHERE jsonb_path_exists(forms, '$.**.style ? (@ == "citation")')
    ) OR EXISTS (
        SELECT 1 FROM lexicon.entry_publications
        WHERE content_schema_version = 3
          AND jsonb_path_exists(snapshot, '$.**.style ? (@ == "citation")')
    ) THEN
        RAISE EXCEPTION 'citation readings must be preserved before rollback';
    END IF;
END $$;
