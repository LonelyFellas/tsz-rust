DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM user_notifications WHERE kind IN ('wordlist_liked', 'wordlist_tipped')) THEN
        RAISE EXCEPTION 'wordlist notifications exist; refusing lossy rollback';
    END IF;
END $$;
ALTER TABLE user_notifications
    DROP CONSTRAINT user_notifications_tip_amount_check,
    DROP CONSTRAINT user_notifications_kind_check,
    DROP COLUMN amount,
    DROP COLUMN actor_user_id,
    DROP COLUMN wordlist_id;
ALTER TABLE user_notifications ADD CONSTRAINT user_notifications_kind_check
    CHECK (kind IN ('teacher_approved', 'teacher_rejected', 'teacher_revoked'));
