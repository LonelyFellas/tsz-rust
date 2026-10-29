-- Keep model review evidence separate from legacy strict question snapshots.
-- NULL explicitly means an older question has no structured review evidence.
ALTER TABLE question_bank ADD COLUMN semantic_review jsonb;
