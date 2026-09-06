ALTER TABLE lexicon.entries
    ADD COLUMN annotation text,
    ADD COLUMN annotation_revision bigint NOT NULL DEFAULT 1,
    ADD CONSTRAINT entries_annotation_length CHECK (annotation IS NULL OR (char_length(annotation) BETWEEN 1 AND 20)),
    ADD CONSTRAINT entries_annotation_revision_positive CHECK (annotation_revision >= 1);
