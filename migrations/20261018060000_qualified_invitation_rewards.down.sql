-- Do not discard pending attribution or resume registration-time issuance.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM invitation_reward_candidates) THEN
        RAISE EXCEPTION 'invitation qualification records exist; retain schema and roll forward';
    END IF;
END $$;
DROP VIEW invitation_reward_records;
DROP TABLE invitation_reward_candidates;
