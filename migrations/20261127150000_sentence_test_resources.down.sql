DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon_next.test_resource_runs r WHERE
        EXISTS (SELECT 1 FROM lexicon_next.test_resource_entries e WHERE e.run_id = r.run_id)
        OR EXISTS (SELECT 1 FROM lexicon_next.test_resource_sentences s WHERE s.run_id = r.run_id)
        OR EXISTS (SELECT 1 FROM lexicon.audio_generations g WHERE g.test_run_id = r.run_id)
        OR EXISTS (SELECT 1 FROM lexicon_next.test_resource_previews p WHERE p.run_id = r.run_id)
        OR EXISTS (SELECT 1 FROM lexicon_next.test_resource_preview_requests p WHERE p.run_id = r.run_id)) THEN
        RAISE EXCEPTION 'purge managed test resources before reverting their registration';
    END IF;
END $$;

DROP TABLE lexicon_next.test_resource_preview_requests;
DROP TABLE lexicon_next.test_resource_previews;
ALTER TABLE lexicon.audio_generations DROP COLUMN test_run_id;
DROP TABLE lexicon_next.test_resource_sentences;
DROP TABLE lexicon_next.test_resource_runs;
