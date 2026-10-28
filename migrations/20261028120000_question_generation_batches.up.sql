CREATE TABLE question_generation_batches (
 id uuid PRIMARY KEY,
 created_by uuid NOT NULL REFERENCES admins(id),
 request_key uuid NOT NULL,
 request_hash bytea NOT NULL,
 selection jsonb NOT NULL,
 preview jsonb NOT NULL,
 cancelled_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(created_by,request_key)
);
ALTER TABLE question_generations ADD COLUMN batch_id uuid REFERENCES question_generation_batches(id);
CREATE INDEX question_generations_batch ON question_generations(batch_id) WHERE batch_id IS NOT NULL;
CREATE INDEX question_generations_coverage ON question_generations((target#>>'{source,entry_id}'),(target#>>'{source,publication_id}'),(target#>>'{source,sense_id}'),(target#>>'{source,definition_id}'),question_level,answer_profile) WHERE state IN ('queued','running');
