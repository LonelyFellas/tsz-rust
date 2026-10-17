-- Keep the immutable skipped attempt, then allow one terminal settlement after
-- same-day wallet recovery. Neither a second award nor repeated skips are allowed.
ALTER TABLE learning_reward_settlements
    DROP CONSTRAINT learning_reward_settlements_user_id_business_day_key;
CREATE UNIQUE INDEX learning_reward_terminal_once
    ON learning_reward_settlements(user_id, business_day)
    WHERE status <> 'wallet_unavailable';
CREATE UNIQUE INDEX learning_reward_wallet_skip_once
    ON learning_reward_settlements(user_id, business_day)
    WHERE status = 'wallet_unavailable';
