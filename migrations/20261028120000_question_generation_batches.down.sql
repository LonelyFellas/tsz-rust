DROP INDEX question_generations_coverage;
ALTER TABLE question_generations DROP COLUMN batch_id;
DROP TABLE question_generation_batches;
