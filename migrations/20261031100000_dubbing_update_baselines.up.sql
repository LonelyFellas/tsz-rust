-- Area keys contain pre-submit groups; missing keys identify unknown historical defaults.
-- _applied_updates retains predecessor batches that actually switched targets.
ALTER TABLE speech.dubbing_audio_updates
    ADD COLUMN baseline_groups JSONB NOT NULL DEFAULT '{}'::jsonb
    CHECK (jsonb_typeof(baseline_groups) = 'object');
