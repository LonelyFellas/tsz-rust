ALTER TABLE dictionary.content_imports
ADD COLUMN source_version TEXT;

UPDATE dictionary.content_imports content_import
SET source_version = dataset.source_version
FROM dictionary.datasets dataset
WHERE dataset.id = content_import.dataset_id;

ALTER TABLE dictionary.content_imports
ALTER COLUMN source_version SET NOT NULL,
ADD CHECK (char_length(btrim(source_version)) > 0);
