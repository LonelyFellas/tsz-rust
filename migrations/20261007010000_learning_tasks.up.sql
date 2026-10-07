CREATE TABLE learning_tasks (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
 question_type text NOT NULL DEFAULT 'spelling_zh_to_en' CHECK(question_type='spelling_zh_to_en'),
 task_type text NOT NULL CHECK (task_type IN ('daily','longterm')),
 wordlist_ids uuid[] NOT NULL CHECK (cardinality(wordlist_ids) BETWEEN 1 AND 5),
 daily_question_count integer, ends_at timestamptz,
 state text NOT NULL DEFAULT 'active' CHECK (state IN ('active','archived')),
 revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
 create_key uuid NOT NULL, create_hash bytea NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(user_id,create_key), UNIQUE(id,user_id),
 CHECK ((task_type='daily' AND daily_question_count IS NOT NULL AND daily_question_count BETWEEN 1 AND 200) OR
        (task_type='longterm' AND daily_question_count IS NULL AND ends_at IS NULL))
);
CREATE TABLE learning_runs (
 id uuid PRIMARY KEY, task_id uuid NOT NULL, user_id uuid NOT NULL,
 task_revision bigint NOT NULL, question_type text NOT NULL DEFAULT 'spelling_zh_to_en' CHECK(question_type='spelling_zh_to_en'),
 task_type text NOT NULL CHECK(task_type IN ('daily','longterm')),
 business_day date, window_start timestamptz, window_end timestamptz, expires_at timestamptz,
 settings_snapshot jsonb NOT NULL, seed uuid NOT NULL,
 target_count integer NOT NULL CHECK(target_count BETWEEN 1 AND 2000),
 state text NOT NULL CHECK(state IN ('active','completed','expired','invalidated','cancelled')),
 generation_version text NOT NULL, grading_version text NOT NULL,
 started_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 FOREIGN KEY(task_id,user_id) REFERENCES learning_tasks(id,user_id) ON DELETE CASCADE,
 UNIQUE(id,task_id,user_id),
 CHECK ((task_type='daily' AND business_day IS NOT NULL AND window_start IS NOT NULL AND window_end IS NOT NULL AND expires_at IS NOT NULL) OR
        (task_type='longterm' AND business_day IS NULL AND expires_at IS NULL))
);
CREATE UNIQUE INDEX learning_daily_active ON learning_runs(task_id,business_day) WHERE state='active' AND task_type='daily';
CREATE UNIQUE INDEX learning_longterm_active ON learning_runs(task_id) WHERE state='active' AND task_type='longterm';
CREATE INDEX learning_run_history ON learning_runs(task_id,started_at DESC,id DESC);
CREATE TABLE learning_questions (
 id uuid PRIMARY KEY, run_id uuid NOT NULL REFERENCES learning_runs(id) ON DELETE CASCADE,
 position integer NOT NULL CHECK(position>=0), source_wordlist_id uuid NOT NULL, source_revision bigint NOT NULL,
 entry_id uuid NOT NULL, entry_archive_generation bigint NOT NULL, publication_id uuid NOT NULL,
 pos_id uuid NOT NULL, sense_id uuid NOT NULL, definition_id uuid NOT NULL, form_ids uuid[] NOT NULL,
 unit_key text NOT NULL, prompt_snapshot jsonb NOT NULL, answer_snapshot jsonb NOT NULL, fingerprint bytea NOT NULL,
 UNIQUE(run_id,position), UNIQUE(run_id,unit_key), UNIQUE(id,run_id)
);
CREATE TABLE learning_answers (
 id uuid PRIMARY KEY, run_id uuid NOT NULL REFERENCES learning_runs(id) ON DELETE CASCADE,
 question_id uuid NOT NULL UNIQUE, request_key uuid NOT NULL, request_hash bytea NOT NULL,
 submitted_answer text NOT NULL, normalized_answer text NOT NULL, is_correct boolean NOT NULL,
 accepted_at timestamptz NOT NULL, grading_version text NOT NULL,
 FOREIGN KEY(question_id,run_id) REFERENCES learning_questions(id,run_id) ON DELETE CASCADE,
 UNIQUE(run_id,request_key)
);
CREATE TABLE learning_completions (
 id uuid PRIMARY KEY, run_id uuid NOT NULL UNIQUE, task_id uuid NOT NULL, user_id uuid NOT NULL,
 question_type text NOT NULL DEFAULT 'spelling_zh_to_en' CHECK(question_type='spelling_zh_to_en'),
 task_type text NOT NULL CHECK(task_type IN ('daily','longterm')), business_day date,
 task_revision bigint NOT NULL, target_count integer NOT NULL CHECK(target_count>0),
 answered_count integer NOT NULL, correct_count integer NOT NULL,
 completed_at timestamptz NOT NULL, generation_version text NOT NULL, grading_version text NOT NULL,
 FOREIGN KEY(run_id,task_id,user_id) REFERENCES learning_runs(id,task_id,user_id) ON DELETE CASCADE,
 CHECK(answered_count=target_count AND correct_count BETWEEN 0 AND answered_count),
 CHECK((task_type='daily')=(business_day IS NOT NULL)), UNIQUE(task_id,business_day)
);
CREATE TABLE learning_run_start_requests (
 user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE, request_key uuid NOT NULL,
 task_id uuid NOT NULL, request_hash bytea NOT NULL, run_id uuid NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), PRIMARY KEY(user_id,request_key),
 FOREIGN KEY(run_id,task_id,user_id) REFERENCES learning_runs(id,task_id,user_id) ON DELETE CASCADE
);
