CREATE TABLE learning_memory_history_months (
    memory_id uuid NOT NULL,
    user_id uuid NOT NULL,
    month date NOT NULL,
    source_kind text NOT NULL CHECK(source_kind IN ('task','review')),
    attempt_count bigint NOT NULL CHECK(attempt_count > 0),
    correct_count bigint NOT NULL CHECK(correct_count BETWEEN 0 AND attempt_count),
    spaced_correct_count bigint NOT NULL CHECK(spaced_correct_count BETWEEN 0 AND correct_count),
    PRIMARY KEY(memory_id,month,source_kind),
    FOREIGN KEY(memory_id,user_id) REFERENCES learning_memories(id,user_id) ON DELETE CASCADE
);
CREATE INDEX learning_memory_events_retention ON learning_memory_events(accepted_at,user_id);
CREATE INDEX learning_review_session_retention ON learning_review_sessions(expires_at,user_id);
