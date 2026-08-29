ALTER TABLE dictionary.entry_contents
ADD COLUMN forms JSONB NOT NULL DEFAULT '[]'::jsonb
CHECK (jsonb_typeof(forms) = 'array'),
ADD COLUMN sounds JSONB NOT NULL DEFAULT '[]'::jsonb
CHECK (jsonb_typeof(sounds) = 'array');

ALTER TABLE dictionary.content_imports
ADD COLUMN parser_version TEXT NOT NULL DEFAULT 'senses-only-v1'
CHECK (char_length(btrim(parser_version)) > 0);
