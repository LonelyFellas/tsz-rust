ALTER TABLE learning_tasks DROP CONSTRAINT learning_tasks_check1;
ALTER TABLE learning_tasks ADD CONSTRAINT learning_tasks_check1
 CHECK (choice_policy IS NULL OR (choice_policy IN ('strict_tiered_v1','mixed_tiered_v1') AND question_type='choice_zh_to_en'));

ALTER TABLE learning_tasks ADD COLUMN direct_practice_key bytea;
CREATE UNIQUE INDEX learning_tasks_active_direct_practice_key
 ON learning_tasks(user_id,direct_practice_key) WHERE state='active' AND direct_practice_key IS NOT NULL;

CREATE TABLE learning_plan_reuse_receipts (
 user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 request_key uuid NOT NULL,
 request_hash bytea NOT NULL,
 task_id uuid NOT NULL REFERENCES learning_tasks(id) ON DELETE CASCADE,
 PRIMARY KEY(user_id,request_key)
);
