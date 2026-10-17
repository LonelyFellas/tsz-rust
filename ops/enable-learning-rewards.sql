-- Common activation day for login (1 coin) and learning rewards.
-- Set LOGIN_REWARD_ENABLED=true on all API instances before the scheduled day.
-- No login award is issued before the enabled policy becomes effective.
-- Approved rule: 10 distinct completed daily units earn 5 coins, once per user/day.
-- Run explicitly with psql -X -v ON_ERROR_STOP=1 -f ops/enable-learning-rewards.sql.
-- No historical rewards are backfilled. The first active business day starts at
-- the next Asia/Shanghai 04:00. Existing policies require an explicit new decision.
BEGIN;
SELECT pg_advisory_xact_lock(72120400);
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM learning_reward_policies) THEN
        RAISE EXCEPTION 'reward policies already exist; inspect before scheduling a change';
    END IF;
    INSERT INTO learning_reward_policies
        (id, rule_version, effective_business_day, enabled, daily_amount, minimum_units)
    VALUES (gen_random_uuid(), 'daily-learning-5-for-10-v1',
        ((clock_timestamp() AT TIME ZONE 'Asia/Shanghai') - interval '4 hours')::date + 1,
        true, 5, 10);
END;
$$;
COMMIT;
