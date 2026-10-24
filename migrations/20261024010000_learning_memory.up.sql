CREATE TABLE learning_memory_enrollments (
    user_id uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    enabled boolean NOT NULL DEFAULT false,
    enabled_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE learning_memories (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    entry_id uuid NOT NULL,
    pos_id uuid NOT NULL,
    sense_id uuid NOT NULL,
    skill text NOT NULL CHECK (skill = 'spelling_zh_to_en'),
    answer_profile text NOT NULL CHECK (answer_profile IN ('BrE', 'AmE')),
    memory_epoch bigint NOT NULL CHECK (memory_epoch > 0),
    content_signature bytea NOT NULL CHECK (octet_length(content_signature) = 32),
    scheduler_version text NOT NULL CHECK (scheduler_version = 'memory_ladder_v1'),
    stage text NOT NULL CHECK (stage IN ('unverified', 'learning', 'reviewing', 'relearning')),
    interval_step smallint NOT NULL CHECK (interval_step BETWEEN 0 AND 6),
    due_at timestamptz NOT NULL,
    last_feedback_at timestamptz,
    last_evidence_at timestamptz NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    paused boolean NOT NULL DEFAULT false,
    content_available boolean NOT NULL DEFAULT true,
    checked_publication_id uuid,
    checked_cefr_level text,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (user_id, entry_id, pos_id, sense_id, skill, answer_profile),
    UNIQUE (id, user_id),
    CHECK (stage = 'reviewing' OR interval_step = 0),
    CHECK (stage = 'unverified' OR last_feedback_at IS NOT NULL)
);
CREATE INDEX learning_memory_due
    ON learning_memories(user_id, answer_profile, due_at, id) WHERE NOT paused AND content_available;

CREATE TABLE learning_memory_sources (
    memory_id uuid NOT NULL,
    user_id uuid NOT NULL,
    wordlist_id uuid NOT NULL,
    membership_id uuid NOT NULL,
    public_generation bigint,
    entry_archive_generation bigint NOT NULL,
    observed_at timestamptz NOT NULL,
    PRIMARY KEY (memory_id, wordlist_id),
    FOREIGN KEY (memory_id, user_id) REFERENCES learning_memories(id, user_id) ON DELETE CASCADE
);
CREATE INDEX learning_memory_source_wordlist ON learning_memory_sources(wordlist_id, memory_id);

-- Reference existing answer facts; do not duplicate their prompt/answer snapshots.
CREATE TABLE learning_memory_events (
    answer_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    memory_id uuid,
    memory_epoch bigint,
    previous_revision bigint,
    resulting_revision bigint,
    content_signature bytea NOT NULL CHECK (octet_length(content_signature) = 32),
    scheduler_version text NOT NULL CHECK (scheduler_version = 'memory_ladder_v1'),
    effect text NOT NULL CHECK (effect IN (
        'initial_correct', 'initial_incorrect', 'review_passed', 'relearning_passed',
        'lapsed', 'relearning_failed', 'practice_early', 'practice_only_content_changed'
    )),
    is_correct boolean NOT NULL,
    accepted_at timestamptz NOT NULL,
    FOREIGN KEY (memory_id, user_id) REFERENCES learning_memories(id, user_id) ON DELETE CASCADE,
    CHECK ((memory_id IS NULL AND memory_epoch IS NULL AND previous_revision IS NULL AND resulting_revision IS NULL)
        OR (memory_id IS NOT NULL AND memory_epoch > 0 AND previous_revision >= 0 AND resulting_revision >= previous_revision))
);
CREATE INDEX learning_memory_events_history ON learning_memory_events(memory_id, accepted_at, answer_id);
