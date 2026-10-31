DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM learning_question_audio) THEN
        RAISE EXCEPTION 'learning audio facts exist; downgrade requires an explicit retention plan';
    END IF;
END $$;
DROP TABLE learning_question_audio;
