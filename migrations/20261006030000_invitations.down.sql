LOCK TABLE invitation_registrations, invitation_codes IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM invitation_codes) OR EXISTS (SELECT 1 FROM invitation_registrations) THEN
        RAISE EXCEPTION 'invitation codes and attribution records must be retained';
    END IF;
END $$;
DROP TABLE invitation_registrations;
DROP TABLE invitation_codes;
DROP FUNCTION preserve_invitation_record();
