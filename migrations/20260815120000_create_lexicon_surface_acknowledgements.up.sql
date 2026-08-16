CREATE TABLE lexicon.entry_surface_acknowledgements (
    entry_id UUID PRIMARY KEY
        REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    detection_id UUID NOT NULL UNIQUE,
    headwords_content_digest TEXT NOT NULL,
    match_ids TEXT[] NOT NULL,
    match_digest TEXT NOT NULL,
    acknowledged_by_admin_id UUID NOT NULL
        REFERENCES admins(id) ON DELETE RESTRICT,
    acknowledged_at TIMESTAMPTZ NOT NULL,
    policy_name TEXT NOT NULL
        CHECK (policy_name IN (
            'surface_warning_acknowledgement',
            'allow_new_exact_headword_entries'
        )),
    policy_epoch BIGINT NOT NULL CHECK (policy_epoch > 0),
    normalization_version INTEGER NOT NULL CHECK (normalization_version > 0),
    CHECK (cardinality(match_ids) > 0)
);

COMMENT ON TABLE lexicon.entry_surface_acknowledgements IS
    'Immutable evidence for acknowledged surface warnings consumed by entry creation.';

ALTER TABLE lexicon.consumed_detections
    ADD CONSTRAINT consumed_detections_detection_id_key UNIQUE (detection_id);
