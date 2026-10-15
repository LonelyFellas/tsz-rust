ALTER TABLE user_notifications DROP CONSTRAINT user_notifications_kind_check;
ALTER TABLE user_notifications
    ADD CONSTRAINT user_notifications_kind_check CHECK (kind IN (
        'teacher_approved', 'teacher_rejected', 'teacher_revoked', 'wordlist_liked', 'wordlist_tipped'
    )),
    ADD COLUMN wordlist_id UUID REFERENCES wordlists(id) ON DELETE SET NULL,
    ADD COLUMN actor_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN amount BIGINT CHECK (amount > 0),
    ADD CONSTRAINT user_notifications_tip_amount_check CHECK ((kind = 'wordlist_tipped') = (amount IS NOT NULL));
