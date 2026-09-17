-- New JSON contracts require the new reader, even when a binding set is empty.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.entry_editor_projection
               WHERE jsonb_path_exists(meanings, '$.**.form_group_ids'))
       OR EXISTS (SELECT 1 FROM lexicon.entry_publications
                  WHERE jsonb_path_exists(snapshot, '$.**.form_group_ids')) THEN
        RAISE EXCEPTION 'new binding JSON must be explicitly downgraded before rollback';
    END IF;
END $$;
DROP TABLE lexicon.sense_form_group_bindings;
ALTER TABLE lexicon.senses DROP CONSTRAINT lexicon_senses_id_pos_entry_key;
