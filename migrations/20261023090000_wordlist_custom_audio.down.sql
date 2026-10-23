DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM wordlist_audio_jobs) OR EXISTS (SELECT 1 FROM wordlist_update_requests) THEN
        RAISE EXCEPTION 'custom audio jobs and save receipts must be retained; refusing destructive rollback';
    END IF;
END $$;
DROP TABLE wordlist_update_requests;
DROP TABLE wordlist_audio_review_refs;
DROP TABLE wordlist_audio_objects;
DROP TABLE wordlist_audio_jobs;
