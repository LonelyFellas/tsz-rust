DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM learning_reward_policies) OR EXISTS(SELECT 1 FROM learning_reward_settlements) THEN
        RAISE EXCEPTION 'learning reward records exist; refusing destructive rollback';
    END IF;
END $$;
DROP INDEX learning_completions_reward_day;
DROP TABLE learning_reward_settlements, learning_reward_policies;
DROP FUNCTION schedule_learning_reward_policy(), preserve_learning_reward_record();
