-- 管理端首页看板的窗口聚合读取；避免在 coin_entries / learning_* 上全表扫描。
CREATE INDEX coin_entries_created_at_idx ON coin_entries (created_at);
CREATE INDEX learning_completions_completed_at_idx ON learning_completions (completed_at);
CREATE INDEX learning_runs_started_at_idx ON learning_runs (started_at);
CREATE INDEX learning_runs_business_day_idx
    ON learning_runs (business_day) WHERE business_day IS NOT NULL;
CREATE INDEX learning_answers_accepted_at_idx ON learning_answers (accepted_at);
CREATE INDEX lexicon_entries_created_at_live_idx
    ON lexicon.entries (created_at) WHERE archived_at IS NULL;
