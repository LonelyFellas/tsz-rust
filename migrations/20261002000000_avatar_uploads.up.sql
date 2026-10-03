CREATE TABLE avatar_uploads (
    id UUID PRIMARY KEY,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    source_key TEXT NOT NULL UNIQUE,
    declared_type TEXT NOT NULL CHECK (declared_type IN ('image/jpeg','image/png','image/webp')),
    size_bytes BIGINT NOT NULL CHECK (size_bytes BETWEEN 1 AND 5242880),
    expires_at TIMESTAMPTZ NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','confirmed','invalid')),
    canonical_key TEXT UNIQUE,
    confirmed_at TIMESTAMPTZ,
    baseline_frozen BOOLEAN NOT NULL DEFAULT false,
    baseline_avatar_upload_id UUID,
    CHECK (baseline_frozen OR baseline_avatar_upload_id IS NULL),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (id,user_id),
    CHECK ((state = 'confirmed') = (canonical_key IS NOT NULL AND confirmed_at IS NOT NULL)),
    CHECK (state = 'confirmed' OR (canonical_key IS NULL AND confirmed_at IS NULL))
);
CREATE INDEX avatar_uploads_owner ON avatar_uploads(user_id,created_at);
CREATE INDEX avatar_uploads_pending ON avatar_uploads(user_id,expires_at) WHERE state = 'pending';
ALTER TABLE users ADD COLUMN avatar_upload_id UUID;
ALTER TABLE users ADD CONSTRAINT users_avatar_owner_fk
    FOREIGN KEY (avatar_upload_id,id) REFERENCES avatar_uploads(id,user_id);
CREATE TABLE avatar_cleanup_tasks (
    id UUID PRIMARY KEY,
    upload_id UUID REFERENCES avatar_uploads(id) ON DELETE SET NULL,
    object_key TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('source','candidate','canonical')),
    not_before TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','cancelled','done')),
    lease_until TIMESTAMPTZ,
    lease_token UUID,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    write_completed BOOLEAN NOT NULL DEFAULT false,
    last_error TEXT CHECK (last_error IN ('storage_unavailable','currently_referenced')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (object_key,kind)
);
CREATE INDEX avatar_cleanup_upload ON avatar_cleanup_tasks(upload_id,kind);
CREATE INDEX avatar_cleanup_due ON avatar_cleanup_tasks(not_before,id) WHERE status = 'pending';
CREATE INDEX avatar_cleanup_recheck ON avatar_cleanup_tasks(not_before,id)
    WHERE status = 'done' AND kind IN ('source','candidate') AND NOT write_completed;
CREATE FUNCTION avatar_cleanup_attempts_monotonic() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.attempts < OLD.attempts THEN
        RAISE EXCEPTION 'avatar cleanup attempts cannot decrease' USING ERRCODE = '23514';
    END IF;
    IF OLD.write_completed AND NOT NEW.write_completed THEN
        RAISE EXCEPTION 'avatar write completion cannot regress' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER avatar_cleanup_attempts_monotonic BEFORE UPDATE ON avatar_cleanup_tasks
    FOR EACH ROW EXECUTE FUNCTION avatar_cleanup_attempts_monotonic();

CREATE FUNCTION avatar_schedule_user_cleanup(owner_id UUID) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    PERFORM id FROM users WHERE id = owner_id FOR UPDATE;
    INSERT INTO avatar_cleanup_tasks (id,upload_id,object_key,kind,not_before)
        SELECT gen_random_uuid(),id,source_key,'source',expires_at + interval '150 seconds'
        FROM avatar_uploads WHERE user_id = owner_id
        ON CONFLICT (object_key,kind) DO NOTHING;
    INSERT INTO avatar_cleanup_tasks (id,upload_id,object_key,kind,not_before,write_completed)
        SELECT gen_random_uuid(),id,canonical_key,'canonical',clock_timestamp(),true
        FROM avatar_uploads WHERE user_id = owner_id AND canonical_key IS NOT NULL
        ON CONFLICT (object_key,kind) DO NOTHING;
END;
$$;
CREATE FUNCTION avatar_user_delete_cleanup() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM avatar_schedule_user_cleanup(OLD.id);
    RETURN OLD;
END;
$$;
CREATE TRIGGER avatar_user_delete_cleanup BEFORE DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION avatar_user_delete_cleanup();
