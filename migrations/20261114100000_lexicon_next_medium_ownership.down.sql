-- Removes only ownership metadata; all draft text and revision snapshots remain intact.
DROP INDEX lexicon_next.lexicon_next_entries_language_medium;
ALTER TABLE lexicon_next.entries DROP COLUMN medium_locale;
