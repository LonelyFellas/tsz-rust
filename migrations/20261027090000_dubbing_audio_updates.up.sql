CREATE TABLE speech.dubbing_update_settings (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    wait_seconds INTEGER NOT NULL DEFAULT 60 CHECK (wait_seconds BETWEEN 0 AND 3600),
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0)
);
INSERT INTO speech.dubbing_update_settings(singleton) VALUES(TRUE);

ALTER TABLE lexicon.audio_generations ADD COLUMN effect_hash BYTEA;
ALTER TABLE lexicon.audio_generations ADD CONSTRAINT audio_generations_effect_hash_length
    CHECK (effect_hash IS NULL OR octet_length(effect_hash) = 32);

CREATE TABLE speech.dubbing_audio_updates (
    id UUID PRIMARY KEY,
    group_id UUID NOT NULL,
    group_name TEXT NOT NULL,
    group_revision BIGINT NOT NULL,
    areas TEXT[] NOT NULL,
    created_by_admin_id UUID NOT NULL REFERENCES admins(id),
    security_version BIGINT NOT NULL,
    test_run TEXT CHECK (test_run IS NULL OR test_run ~ '^[a-z0-9][a-z0-9_-]{7,79}$'),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN
        ('pending','running','completed','failed','partial','cancelled','superseded')),
    due_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX dubbing_audio_updates_active_group
    ON speech.dubbing_audio_updates(group_id) WHERE state IN ('pending','running');
CREATE INDEX dubbing_audio_updates_due ON speech.dubbing_audio_updates(due_at) WHERE state = 'pending';

CREATE TABLE speech.dubbing_audio_update_targets (
    id UUID PRIMARY KEY,
    update_id UUID NOT NULL REFERENCES speech.dubbing_audio_updates(id) ON DELETE CASCADE,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('entry','sentence')),
    owner_id UUID NOT NULL,
    target_id UUID NOT NULL,
    target_path TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','running','completed','failed','skipped')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    lease_token UUID,
    lease_until TIMESTAMPTZ,
    retry_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    error_code TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(update_id,owner_kind,owner_id,target_id,target_path)
);
CREATE INDEX dubbing_audio_update_targets_ready
    ON speech.dubbing_audio_update_targets(retry_at,lease_until) WHERE state IN ('pending','running');
