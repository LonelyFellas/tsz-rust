DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM learning_memory_history_months) THEN
        RAISE EXCEPTION 'personal memory history summaries exist; preserve the schema';
    END IF;
END $$;
DROP INDEX learning_review_session_retention;
DROP INDEX learning_memory_events_retention;
DROP TABLE learning_memory_history_months;
