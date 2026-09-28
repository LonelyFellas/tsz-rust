CREATE TABLE teacher_applications (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    real_name TEXT NOT NULL CHECK (char_length(btrim(real_name)) BETWEEN 1 AND 50),
    contact TEXT NOT NULL CHECK (char_length(btrim(contact)) BETWEEN 1 AND 254),
    statement TEXT NOT NULL CHECK (char_length(btrim(statement)) BETWEEN 1 AND 2000),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected', 'revoked')),
    submitted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    reviewed_at TIMESTAMPTZ,
    reviewed_by UUID REFERENCES admins(id) ON DELETE SET NULL,
    review_reason TEXT,
    revoked_at TIMESTAMPTZ,
    revoked_by UUID REFERENCES admins(id) ON DELETE SET NULL,
    revoke_reason TEXT,
    CHECK (status <> 'rejected' OR (review_reason IS NOT NULL AND char_length(btrim(review_reason)) > 0)),
    CHECK (status <> 'revoked' OR (revoke_reason IS NOT NULL AND char_length(btrim(revoke_reason)) > 0))
);
CREATE UNIQUE INDEX teacher_applications_pending_user ON teacher_applications(user_id) WHERE status = 'pending';
CREATE UNIQUE INDEX teacher_applications_approved_user ON teacher_applications(user_id) WHERE status = 'approved';
CREATE INDEX teacher_applications_user_created ON teacher_applications(user_id, submitted_at DESC, id DESC);
CREATE INDEX teacher_applications_queue ON teacher_applications(status, submitted_at, id);

CREATE TABLE teacher_certification_files (
    id UUID PRIMARY KEY,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    kind TEXT NOT NULL CHECK (kind IN ('id_front', 'id_back', 'education', 'language')),
    object_key TEXT NOT NULL UNIQUE,
    content_type TEXT NOT NULL CHECK (content_type IN ('image/jpeg', 'image/png', 'image/webp')),
    size_bytes BIGINT NOT NULL CHECK (size_bytes BETWEEN 1 AND 10485760),
    state TEXT NOT NULL DEFAULT 'uploading' CHECK (state IN ('uploading', 'ready', 'delete_pending')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ DEFAULT now() + interval '24 hours'
);
CREATE INDEX teacher_files_cleanup ON teacher_certification_files(expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX teacher_files_owner ON teacher_certification_files(user_id);

CREATE TABLE teacher_application_files (
    application_id UUID NOT NULL REFERENCES teacher_applications(id) ON DELETE CASCADE,
    file_id UUID NOT NULL REFERENCES teacher_certification_files(id),
    position SMALLINT NOT NULL CHECK (position >= 0),
    PRIMARY KEY (application_id, file_id),
    UNIQUE (application_id, position)
);
CREATE INDEX teacher_application_files_file ON teacher_application_files(file_id);

CREATE TABLE user_notifications (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    application_id UUID REFERENCES teacher_applications(id) ON DELETE SET NULL,
    kind TEXT NOT NULL CHECK (kind IN ('teacher_approved', 'teacher_rejected', 'teacher_revoked')),
    reason TEXT,
    actor_id UUID REFERENCES admins(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    read_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX user_notifications_application_event ON user_notifications(application_id, kind) WHERE application_id IS NOT NULL;
CREATE INDEX user_notifications_user_created ON user_notifications(user_id, created_at DESC, id DESC);
