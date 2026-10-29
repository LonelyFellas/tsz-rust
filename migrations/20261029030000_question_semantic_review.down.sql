-- Only model-review evidence is removed; question and learning snapshots remain.
ALTER TABLE question_bank DROP COLUMN semantic_review;
