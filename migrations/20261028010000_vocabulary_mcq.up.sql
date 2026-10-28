-- Question snapshots are immutable learning evidence; drafts require human approval.
ALTER TABLE lexicon.entries ADD COLUMN teaching_eligible boolean NOT NULL DEFAULT true;
CREATE TABLE question_generations (
 id uuid PRIMARY KEY, created_by uuid NOT NULL REFERENCES admins(id), security_version bigint NOT NULL,
 permission_version bigint NOT NULL, request_key uuid NOT NULL, request_hash bytea NOT NULL,
 target jsonb NOT NULL, question_level text NOT NULL CHECK(question_level IN ('A1','A2','B1','B2','C1','C2')),
 answer_profile text NOT NULL CHECK(answer_profile IN ('BrE','AmE')),
 state text NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','running','succeeded','failed','cancelled','insufficient_candidates')),
 revision bigint NOT NULL DEFAULT 1, agent_run_id uuid, question_id uuid, error_code text,
 reasons text[] NOT NULL DEFAULT '{}', expires_at timestamptz NOT NULL DEFAULT now()+interval '2 hours',
 next_attempt_at timestamptz NOT NULL DEFAULT now(), attempts integer NOT NULL DEFAULT 0,
 cancel_delivered boolean NOT NULL DEFAULT false, submitted_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(created_by,request_key)
);
CREATE INDEX question_generations_due ON question_generations(next_attempt_at) WHERE state IN ('queued','running','cancelled');
CREATE TABLE question_bank (
 id uuid PRIMARY KEY, generation_id uuid NOT NULL UNIQUE REFERENCES question_generations(id),
 revision bigint NOT NULL DEFAULT 1, state text NOT NULL DEFAULT 'draft' CHECK(state IN ('draft','approved','rejected','disabled')),
 content jsonb NOT NULL, original_content jsonb NOT NULL,
 target_entry_id uuid NOT NULL, target_sense_id uuid NOT NULL, target_publication_id uuid NOT NULL,
 question_level text NOT NULL, answer_profile text NOT NULL,
 created_by uuid NOT NULL REFERENCES admins(id), reviewed_by uuid REFERENCES admins(id), review_comment text NOT NULL DEFAULT '',
 review_id uuid NOT NULL, question_revision_id uuid NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX question_bank_ready ON question_bank(target_entry_id,target_sense_id,question_level,answer_profile) WHERE state='approved';
CREATE TABLE question_review_deliveries (
 event_id uuid PRIMARY KEY, question_id uuid NOT NULL REFERENCES question_bank(id), payload jsonb NOT NULL,
 delivered_at timestamptz, attempts integer NOT NULL DEFAULT 0, next_attempt_at timestamptz NOT NULL DEFAULT now(),
 created_at timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE learning_tasks DROP CONSTRAINT learning_tasks_question_type_check;
ALTER TABLE learning_tasks ADD CONSTRAINT learning_tasks_question_type_check CHECK(question_type IN ('spelling_zh_to_en','choice_zh_to_en'));
ALTER TABLE learning_runs DROP CONSTRAINT learning_runs_question_type_check;
ALTER TABLE learning_runs ADD CONSTRAINT learning_runs_question_type_check CHECK(question_type IN ('spelling_zh_to_en','choice_zh_to_en'));
ALTER TABLE learning_completions DROP CONSTRAINT learning_completions_question_type_check;
ALTER TABLE learning_completions ADD CONSTRAINT learning_completions_question_type_check CHECK(question_type IN ('spelling_zh_to_en','choice_zh_to_en'));
