-- Restoring the old identity is safe only before any recovered day has two rows.
-- ADD CONSTRAINT validates existing rows and rejects instead of deleting history.
ALTER TABLE learning_reward_settlements
    ADD CONSTRAINT learning_reward_settlements_user_id_business_day_key
    UNIQUE (user_id, business_day);
DROP INDEX learning_reward_terminal_once;
DROP INDEX learning_reward_wallet_skip_once;
