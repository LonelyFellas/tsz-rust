CREATE INDEX audio_generations_owned_effect_ready_idx
    ON lexicon.audio_generations(created_by_admin_id, effect_hash)
    WHERE state = 'ready' AND effect_hash IS NOT NULL;
