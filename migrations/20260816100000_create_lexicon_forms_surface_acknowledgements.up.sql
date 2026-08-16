CREATE TABLE lexicon.entry_forms_surface_acknowledgements (
    entry_id UUID PRIMARY KEY
        REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    forms_revision BIGINT NOT NULL CHECK (forms_revision > 0),
    forms_content_digest TEXT NOT NULL,
    match_ids TEXT[] NOT NULL,
    match_digest TEXT NOT NULL,
    acknowledged_by_admin_id UUID NOT NULL
        REFERENCES admins(id) ON DELETE RESTRICT,
    acknowledged_at TIMESTAMPTZ NOT NULL,
    policy_name TEXT NOT NULL
        CHECK (policy_name = 'surface_warning_acknowledgement'),
    policy_epoch BIGINT NOT NULL CHECK (policy_epoch > 0),
    normalization_version INTEGER NOT NULL CHECK (normalization_version > 0),
    CHECK (cardinality(match_ids) > 0)
);

COMMENT ON TABLE lexicon.entry_forms_surface_acknowledgements IS
    'Reusable evidence for acknowledged cross-entry surface warnings on canonical forms content.';
