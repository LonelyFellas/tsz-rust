DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM teacher_applications)
        OR EXISTS (SELECT 1 FROM teacher_certification_files)
        OR EXISTS (SELECT 1 FROM user_notifications) THEN
        RAISE EXCEPTION 'teacher certification data must be retained; rollback refused';
    END IF;
END $$;

DROP TABLE user_notifications;
DROP TABLE teacher_application_files;
DROP TABLE teacher_certification_files;
DROP TABLE teacher_applications;
