-- New registrations wait for the invitee's first successful daily learning reward.
-- Historical terminal registrations remain immutable and are never backfilled.
CREATE TABLE invitation_reward_candidates (
    invitee_user_id uuid PRIMARY KEY,
    inviter_user_id uuid NOT NULL REFERENCES invitation_codes(user_id),
    reward_amount bigint NOT NULL CHECK (reward_amount > 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (invitee_user_id <> inviter_user_id)
);
CREATE INDEX invitation_reward_candidates_inviter
    ON invitation_reward_candidates(inviter_user_id, created_at DESC, invitee_user_id);
CREATE TRIGGER invitation_reward_candidates_immutable BEFORE UPDATE OR DELETE
    ON invitation_reward_candidates FOR EACH ROW EXECUTE FUNCTION preserve_invitation_record();

CREATE VIEW invitation_reward_records AS
SELECT c.invitee_user_id, c.inviter_user_id,
       coalesce(r.reward_status, 'pending_qualification') AS reward_status,
       coalesce(r.reward_amount, 0) AS reward_amount, c.created_at
FROM invitation_reward_candidates c
LEFT JOIN invitation_registrations r ON r.invitee_user_id=c.invitee_user_id
UNION ALL
SELECT r.invitee_user_id,r.inviter_user_id,r.reward_status,r.reward_amount,r.created_at
FROM invitation_registrations r
WHERE NOT EXISTS (SELECT 1 FROM invitation_reward_candidates c WHERE c.invitee_user_id=r.invitee_user_id);
