DROP INDEX learning_memory_due;
CREATE INDEX learning_memory_due
    ON learning_memories(user_id, answer_profile, due_at, id) WHERE NOT paused AND content_available;
