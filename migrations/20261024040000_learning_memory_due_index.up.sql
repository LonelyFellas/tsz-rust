-- Candidate reads also recheck unavailable content after publication changes.
-- Index all unpaused memories so this predicate retains user/due ordering.
DROP INDEX learning_memory_due;
CREATE INDEX learning_memory_due
    ON learning_memories(user_id, answer_profile, due_at, id) WHERE NOT paused;
