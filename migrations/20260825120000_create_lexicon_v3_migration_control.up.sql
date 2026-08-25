-- Operational control plane for deterministic V2 -> V3 current-draft migration.
--
-- These tables never replace the canonical entry/form tables. They retain the
-- dry-run digest, immutable-publication fingerprint, node mapping and the
-- original V2 editor projection needed for a narrowly bounded rollback.

CREATE TABLE lexicon.v3_migration_batches (
    id UUID PRIMARY KEY,
    source_schema_version SMALLINT NOT NULL DEFAULT 2
        CONSTRAINT lexicon_v3_migration_batches_source_version_check
        CHECK (source_schema_version = 2),
    target_schema_version SMALLINT NOT NULL DEFAULT 3
        CONSTRAINT lexicon_v3_migration_batches_target_version_check
        CHECK (target_schema_version = 3),
    status TEXT NOT NULL
        CONSTRAINT lexicon_v3_migration_batches_status_check
        CHECK (status IN (
            'planned', 'approved', 'applying', 'applied', 'verified',
            'rolled_back', 'failed'
        )),
    selection_digest BYTEA NOT NULL,
    manifest_digest BYTEA NOT NULL,
    requested_by_admin_id UUID NOT NULL
        CONSTRAINT lexicon_v3_migration_batches_admin_fkey
        REFERENCES admins(id) ON DELETE RESTRICT,
    request_id UUID NOT NULL,
    approved_by_admin_id UUID
        CONSTRAINT lexicon_v3_migration_batches_approved_admin_fkey
        REFERENCES admins(id) ON DELETE RESTRICT,
    approval_request_id UUID,
    approved_at TIMESTAMPTZ,
    scanned_count INTEGER NOT NULL DEFAULT 0
        CONSTRAINT lexicon_v3_migration_batches_scanned_check CHECK (scanned_count >= 0),
    eligible_count INTEGER NOT NULL DEFAULT 0
        CONSTRAINT lexicon_v3_migration_batches_eligible_check CHECK (eligible_count >= 0),
    applied_count INTEGER NOT NULL DEFAULT 0
        CONSTRAINT lexicon_v3_migration_batches_applied_check CHECK (applied_count >= 0),
    blocked_count INTEGER NOT NULL DEFAULT 0
        CONSTRAINT lexicon_v3_migration_batches_blocked_check CHECK (blocked_count >= 0),
    failed_count INTEGER NOT NULL DEFAULT 0
        CONSTRAINT lexicon_v3_migration_batches_failed_check CHECK (failed_count >= 0),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    CONSTRAINT lexicon_v3_migration_batches_approval_shape_check CHECK (
        (
            status = 'planned'
            AND approved_by_admin_id IS NULL
            AND approval_request_id IS NULL
            AND approved_at IS NULL
        )
        OR (
            status <> 'planned'
            AND approved_by_admin_id IS NOT NULL
            AND approval_request_id IS NOT NULL
            AND approved_at IS NOT NULL
        )
    )
);

CREATE INDEX lexicon_v3_migration_batches_status_idx
    ON lexicon.v3_migration_batches (status, started_at, id);

CREATE TABLE lexicon.v3_migration_entries (
    batch_id UUID NOT NULL
        CONSTRAINT lexicon_v3_migration_entries_batch_fkey
        REFERENCES lexicon.v3_migration_batches(id) ON DELETE RESTRICT,
    entry_id UUID NOT NULL
        CONSTRAINT lexicon_v3_migration_entries_entry_fkey
        REFERENCES lexicon.entries(id) ON DELETE RESTRICT,
    status TEXT NOT NULL
        CONSTRAINT lexicon_v3_migration_entries_status_check
        CHECK (status IN ('planned', 'applied', 'verified', 'rolled_back', 'blocked', 'failed')),
    source_revision BIGINT
        CONSTRAINT lexicon_v3_migration_entries_revision_check
        CHECK (source_revision IS NULL OR source_revision > 0),
    source_current_publication_id UUID,
    source_publications_digest BYTEA,
    source_pos_modes JSONB,
    source_forms JSONB,
    source_meanings JSONB,
    source_draft_surfaces JSONB,
    expected_forms JSONB,
    expected_presentation JSONB,
    expected_digest BYTEA,
    applied_digest BYTEA,
    block_code TEXT,
    failure_code TEXT,
    applied_at TIMESTAMPTZ,
    verified_at TIMESTAMPTZ,
    rolled_back_at TIMESTAMPTZ,
    PRIMARY KEY (batch_id, entry_id),
    CONSTRAINT lexicon_v3_migration_entries_publication_fkey
        FOREIGN KEY (source_current_publication_id, entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_migration_entries_payload_shape_check CHECK (
        (
            status IN ('planned', 'applied', 'verified', 'rolled_back')
            AND source_revision IS NOT NULL
            AND source_publications_digest IS NOT NULL
            AND source_pos_modes IS NOT NULL
            AND source_forms IS NOT NULL
            AND source_meanings IS NOT NULL
            AND source_draft_surfaces IS NOT NULL
            AND expected_forms IS NOT NULL
            AND expected_presentation IS NOT NULL
            AND expected_digest IS NOT NULL
            AND block_code IS NULL
            AND failure_code IS NULL
        )
        OR (
            status = 'blocked'
            AND block_code IS NOT NULL
            AND applied_digest IS NULL
            AND failure_code IS NULL
        )
        OR (
            status = 'failed'
            AND failure_code IS NOT NULL
        )
    )
);

-- An entry can be owned by only one live migration. A completed rollback frees
-- it for a later, explicitly new batch.
CREATE UNIQUE INDEX lexicon_v3_migration_entries_live_entry_key
    ON lexicon.v3_migration_entries (entry_id)
    WHERE status IN ('planned', 'applied', 'verified');

CREATE INDEX lexicon_v3_migration_entries_batch_status_idx
    ON lexicon.v3_migration_entries (batch_id, status, entry_id);

CREATE TABLE lexicon.v3_migration_map (
    batch_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    v2_node_id UUID,
    v3_node_id UUID NOT NULL,
    role TEXT NOT NULL
        CONSTRAINT lexicon_v3_migration_map_role_check CHECK (role IN (
            'entry', 'pos', 'form_group', 'synthetic_base_only_group',
            'concrete_form', 'group_membership', 'form_variant', 'pronunciation'
        )),
    mapping_kind TEXT NOT NULL
        CONSTRAINT lexicon_v3_migration_map_kind_check
        CHECK (mapping_kind IN ('preserved', 'deterministic_generated')),
    PRIMARY KEY (batch_id, entry_id, role, v3_node_id),
    CONSTRAINT lexicon_v3_migration_map_entry_fkey
        FOREIGN KEY (batch_id, entry_id)
        REFERENCES lexicon.v3_migration_entries(batch_id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_migration_map_v3_node_entry_key
        UNIQUE (batch_id, entry_id, v3_node_id)
);

CREATE INDEX lexicon_v3_migration_map_v2_idx
    ON lexicon.v3_migration_map (entry_id, v2_node_id, role)
    WHERE v2_node_id IS NOT NULL;
