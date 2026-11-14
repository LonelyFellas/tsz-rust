-- Removes saved language preferences only; identities and all other preferences remain.
ALTER TABLE admins DROP COLUMN lexicon_languages;
