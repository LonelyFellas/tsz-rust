DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon_next.publications) THEN
        RAISE EXCEPTION 'lexicon-next publications exist; refusing data loss or incompatible rollback';
    END IF;
END $$;
DROP TABLE lexicon_next.publication_taxonomy_refs;
DROP TABLE lexicon_next.publication_audio_references;
DROP TABLE lexicon_next.publication_commands;
DROP TABLE lexicon_next.publications;
DROP FUNCTION lexicon_next.reject_publication_update();
ALTER TABLE lexicon_next.entries DROP CONSTRAINT entries_status_check;
ALTER TABLE lexicon_next.entries ADD CONSTRAINT entries_status_check CHECK (status IN ('draft','archived'));
