-- Stable identities intentionally survive physical account deletion.
CREATE TABLE invitation_codes (
    user_id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code ~ '^[A-F0-9]{16}$'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE invitation_registrations (
    invitee_user_id uuid PRIMARY KEY,
    inviter_user_id uuid NOT NULL REFERENCES invitation_codes(user_id),
    reward_status text NOT NULL CHECK (reward_status IN ('awarded', 'reward_disabled', 'inviter_unavailable')),
    reward_amount bigint NOT NULL,
    operation_id uuid UNIQUE REFERENCES coin_operations(id),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (invitee_user_id <> inviter_user_id),
    CHECK (
        (reward_status = 'awarded' AND reward_amount > 0 AND operation_id IS NOT NULL)
        OR (reward_status <> 'awarded' AND reward_amount = 0 AND operation_id IS NULL)
    )
);
CREATE INDEX invitation_registrations_inviter ON invitation_registrations(inviter_user_id, created_at DESC, invitee_user_id);
CREATE FUNCTION preserve_invitation_record() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'invitation records are immutable';
END;
$$;
CREATE TRIGGER invitation_codes_immutable BEFORE UPDATE OR DELETE ON invitation_codes
    FOR EACH ROW EXECUTE FUNCTION preserve_invitation_record();
CREATE TRIGGER invitation_registrations_immutable BEFORE UPDATE OR DELETE ON invitation_registrations
    FOR EACH ROW EXECUTE FUNCTION preserve_invitation_record();
