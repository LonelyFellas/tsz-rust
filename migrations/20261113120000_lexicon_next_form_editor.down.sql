-- Refuse rollback while the extended draft format is still in use.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon_next.draft_revisions WHERE document ? 'form_editor') THEN
        RAISE EXCEPTION 'lexicon-next form editor drafts must be removed before rollback';
    END IF;
END $$;
DROP TABLE lexicon_next.audio_asset_references;
DROP TABLE lexicon_next.form_editor_nodes;
