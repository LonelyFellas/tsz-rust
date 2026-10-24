DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM learning_memories)
        OR EXISTS (SELECT 1 FROM learning_memory_events)
        OR EXISTS (SELECT 1 FROM learning_memory_enrollments) THEN
        RAISE EXCEPTION 'personal learning memory data exists; disable collection and preserve the schema';
    END IF;
END $$;
DROP TABLE learning_memory_events;
DROP TABLE learning_memory_sources;
DROP TABLE learning_memories;
DROP TABLE learning_memory_enrollments;
