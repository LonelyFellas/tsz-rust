DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM learning_review_sessions)
        OR EXISTS(SELECT 1 FROM learning_memory_events WHERE source_kind='review') THEN
        RAISE EXCEPTION 'personal review history exists; disable reviews and preserve the schema';
    END IF;
END $$;
DROP TABLE learning_review_start_requests;
DROP TABLE learning_review_attempts;
DROP TABLE learning_review_items;
DROP TABLE learning_review_sessions;
ALTER TABLE learning_memory_events DROP COLUMN source_kind;
ALTER TABLE learning_memory_events DROP CONSTRAINT learning_memory_events_effect_check;
ALTER TABLE learning_memory_events ADD CONSTRAINT learning_memory_events_effect_check CHECK(effect IN (
    'initial_correct','initial_incorrect','review_passed','relearning_passed','lapsed',
    'relearning_failed','practice_early','practice_only_content_changed'
));
ALTER TABLE learning_memories DROP COLUMN retest_day, DROP COLUMN retest_issued;
