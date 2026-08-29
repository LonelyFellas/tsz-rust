ALTER TABLE dictionary.content_imports
DROP COLUMN parser_version;

ALTER TABLE dictionary.entry_contents
DROP COLUMN sounds,
DROP COLUMN forms;
