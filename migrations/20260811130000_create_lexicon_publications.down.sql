DROP TABLE audit.admin_actions;
DROP SCHEMA audit;

DROP TABLE platform.outbox_events;

DROP TABLE lexicon.entry_publication_sense_refs;
DROP TABLE lexicon.entry_publication_sub_part_of_speech_refs;
DROP TABLE lexicon.entry_publication_part_of_speech_refs;
DROP TABLE lexicon.entry_publication_nodes;

ALTER TABLE lexicon.entries
    DROP CONSTRAINT lexicon_entries_draft_publication_fkey,
    DROP CONSTRAINT lexicon_entries_current_publication_fkey;

DROP TABLE lexicon.entry_publications;
