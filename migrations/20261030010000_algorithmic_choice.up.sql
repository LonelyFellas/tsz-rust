ALTER TABLE question_generations ADD COLUMN generation_method text NOT NULL DEFAULT 'agent';
UPDATE question_generations SET state='cancelled', error_code=NULL, reasons='{}', cancel_delivered=(submitted_at IS NULL), revision=revision+1, updated_at=now(), next_attempt_at=now() WHERE state IN ('queued','running');
UPDATE question_bank SET state='disabled',revision=revision+1,updated_at=now() WHERE state IN ('draft','approved','rejected');
ALTER TABLE question_generations ALTER COLUMN generation_method SET DEFAULT 'algorithm';
