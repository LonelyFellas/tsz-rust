ALTER TABLE lexicon.entries
    DROP CONSTRAINT lexicon_entries_schema_kind_check,
    ADD CONSTRAINT lexicon_entries_schema_kind_check
        CHECK (content_schema_version <> 3 OR kind = 'word');
