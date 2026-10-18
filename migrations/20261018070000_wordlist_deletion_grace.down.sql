DELETE FROM user_notifications WHERE kind = 'wordlist_deleted';

ALTER TABLE user_notifications DROP CONSTRAINT user_notifications_kind_check;
ALTER TABLE user_notifications
    ADD CONSTRAINT user_notifications_kind_check CHECK (kind IN (
        'teacher_approved', 'teacher_rejected', 'teacher_revoked',
        'wordlist_liked', 'wordlist_tipped'
    ));

DROP TABLE wordlist_deletion_events;
