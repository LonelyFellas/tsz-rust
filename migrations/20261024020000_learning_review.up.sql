ALTER TABLE learning_memories
    ADD COLUMN retest_day date,
    ADD COLUMN retest_issued smallint NOT NULL DEFAULT 0 CHECK(retest_issued BETWEEN 0 AND 2);
ALTER TABLE learning_memory_events ADD COLUMN source_kind text NOT NULL DEFAULT 'task' CHECK(source_kind IN ('task','review'));
ALTER TABLE learning_memory_events DROP CONSTRAINT learning_memory_events_effect_check;
ALTER TABLE learning_memory_events ADD CONSTRAINT learning_memory_events_effect_check CHECK(effect IN (
    'initial_correct','initial_incorrect','review_passed','relearning_passed','lapsed',
    'relearning_failed','practice_early','practice_only_content_changed','practice_only_stale','practice_assisted'
));

CREATE TABLE learning_review_sessions (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    mode text NOT NULL CHECK(mode IN ('due','retest')),
    settings_snapshot jsonb NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    CHECK(expires_at > created_at),
    UNIQUE(id,user_id)
);
CREATE TABLE learning_review_items (
    id uuid PRIMARY KEY,
    session_id uuid NOT NULL,
    user_id uuid NOT NULL,
    memory_id uuid NOT NULL,
    memory_revision bigint NOT NULL,
    memory_epoch bigint NOT NULL,
    content_signature bytea NOT NULL,
    source_wordlist_id uuid NOT NULL,
    source_membership_id uuid NOT NULL,
    source_public_generation bigint,
    entry_id uuid NOT NULL,
    entry_archive_generation bigint NOT NULL,
    pos_id uuid NOT NULL,
    sense_id uuid NOT NULL,
    publication_id uuid NOT NULL,
    prompt_snapshot jsonb NOT NULL,
    answer_snapshot jsonb NOT NULL,
    generation_version text NOT NULL,
    grading_version text NOT NULL,
    position integer NOT NULL,
    kind text NOT NULL CHECK(kind IN ('due','retest')),
    issued_day date NOT NULL,
    expires_at timestamptz NOT NULL,
    revealed_at timestamptz,
    active boolean NOT NULL DEFAULT true,
    FOREIGN KEY(session_id,user_id) REFERENCES learning_review_sessions(id,user_id) ON DELETE CASCADE,
    FOREIGN KEY(memory_id,user_id) REFERENCES learning_memories(id,user_id) ON DELETE CASCADE,
    UNIQUE(session_id,memory_id), UNIQUE(session_id,position), UNIQUE(id,session_id,user_id)
);
CREATE UNIQUE INDEX learning_review_retest_active ON learning_review_items(memory_id) WHERE active AND kind='retest';
CREATE INDEX learning_review_active_session ON learning_review_items(user_id,session_id) WHERE active;
CREATE TABLE learning_review_attempts (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id uuid NOT NULL UNIQUE,
    session_id uuid NOT NULL,
    request_key uuid NOT NULL,
    request_hash bytea NOT NULL,
    submitted_answer text NOT NULL,
    is_correct boolean NOT NULL,
    effect text NOT NULL,
    accepted_at timestamptz NOT NULL,
    FOREIGN KEY(item_id,session_id,user_id) REFERENCES learning_review_items(id,session_id,user_id) ON DELETE CASCADE,
    UNIQUE(user_id,request_key)
);
CREATE TABLE learning_review_start_requests (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    request_key uuid NOT NULL,
    request_hash bytea NOT NULL,
    session_id uuid NOT NULL,
    PRIMARY KEY(user_id,request_key),
    FOREIGN KEY(session_id,user_id) REFERENCES learning_review_sessions(id,user_id) ON DELETE CASCADE
);
