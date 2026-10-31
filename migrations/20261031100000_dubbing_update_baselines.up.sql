-- Missing area keys identify old jobs whose pre-submit defaults are unknown.
ALTER TABLE speech.dubbing_audio_updates
    ADD COLUMN baseline_groups JSONB NOT NULL DEFAULT '{}'::jsonb
    CHECK (jsonb_typeof(baseline_groups) = 'object');
