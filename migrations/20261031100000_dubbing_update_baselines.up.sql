-- Area keys contain pre-submit groups; missing keys identify unknown historical defaults.
-- _applied_updates retains predecessor batches with known or possible historical writes.
ALTER TABLE speech.dubbing_audio_updates
    ADD COLUMN baseline_groups JSONB NOT NULL DEFAULT '{}'::jsonb
    CHECK (jsonb_typeof(baseline_groups) = 'object');

-- NULL preserves unknown historical outcomes. Current worker transactions write true/false.
ALTER TABLE speech.dubbing_audio_update_targets ADD COLUMN applied BOOLEAN;
