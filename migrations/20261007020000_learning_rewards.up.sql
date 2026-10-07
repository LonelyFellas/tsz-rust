CREATE TABLE learning_reward_policies (
    id uuid PRIMARY KEY,
    rule_version text NOT NULL UNIQUE CHECK (length(rule_version) BETWEEN 1 AND 100),
    effective_business_day date NOT NULL UNIQUE,
    enabled boolean NOT NULL,
    daily_amount bigint CHECK (daily_amount > 0),
    minimum_units integer CHECK (minimum_units BETWEEN 1 AND 200),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (NOT enabled OR (daily_amount IS NOT NULL AND minimum_units IS NOT NULL))
);

-- Readers take the matching shared transaction lock AFTER all participant account locks.
-- Policy writers never acquire account locks. Holding this through commit closes the
-- 04:00 race between validating a future policy and making it visible to settlements.
CREATE FUNCTION schedule_learning_reward_policy() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(72120400);
    IF NEW.effective_business_day <= ((clock_timestamp() AT TIME ZONE 'Asia/Shanghai') - interval '4 hours')::date THEN
        RAISE EXCEPTION 'learning reward policies must start on a future business day';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER learning_reward_policy_schedule BEFORE INSERT ON learning_reward_policies
    FOR EACH ROW EXECUTE FUNCTION schedule_learning_reward_policy();

CREATE TABLE learning_reward_settlements (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL,
    business_day date NOT NULL,
    policy_id uuid REFERENCES learning_reward_policies(id) ON DELETE RESTRICT,
    rule_version text,
    daily_amount bigint CHECK (daily_amount > 0),
    minimum_units integer CHECK (minimum_units BETWEEN 1 AND 200),
    trigger_completion_id uuid NOT NULL,
    completed_at timestamptz NOT NULL,
    qualifying_units integer NOT NULL CHECK (qualifying_units BETWEEN 0 AND 200),
    status text NOT NULL CHECK (status IN ('awarded', 'reward_disabled', 'wallet_unavailable')),
    awarded_amount bigint NOT NULL,
    operation_id uuid UNIQUE REFERENCES coin_operations(id) ON DELETE RESTRICT,
    settled_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (user_id, business_day),
    CHECK ((policy_id IS NULL AND rule_version IS NULL AND daily_amount IS NULL AND minimum_units IS NULL AND status = 'reward_disabled')
        OR (policy_id IS NOT NULL AND rule_version IS NOT NULL)),
    CHECK ((status = 'reward_disabled' AND qualifying_units = 0)
        OR (status <> 'reward_disabled' AND daily_amount IS NOT NULL AND minimum_units IS NOT NULL AND qualifying_units = minimum_units)),
    CHECK ((status = 'awarded' AND awarded_amount = daily_amount AND awarded_amount > 0 AND operation_id IS NOT NULL)
        OR (status <> 'awarded' AND awarded_amount = 0 AND operation_id IS NULL))
);
-- These minimal audit identities intentionally survive physical account/learning deletion.
CREATE FUNCTION preserve_learning_reward_record() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'learning reward policies and settlements are immutable';
END;
$$;
CREATE TRIGGER learning_reward_policies_immutable BEFORE UPDATE OR DELETE ON learning_reward_policies
    FOR EACH ROW EXECUTE FUNCTION preserve_learning_reward_record();
CREATE TRIGGER learning_reward_settlements_immutable BEFORE UPDATE OR DELETE ON learning_reward_settlements
    FOR EACH ROW EXECUTE FUNCTION preserve_learning_reward_record();
CREATE INDEX learning_completions_reward_day ON learning_completions(user_id, business_day, run_id)
    WHERE task_type = 'daily';
