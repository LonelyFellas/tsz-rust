CREATE SCHEMA audit;

CREATE TABLE lexicon.entry_publications (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL
        CONSTRAINT lexicon_entry_publications_entry_fkey
        REFERENCES lexicon.entries(id) ON DELETE RESTRICT,
    publication_number INTEGER NOT NULL
        CONSTRAINT lexicon_entry_publications_number_check CHECK (publication_number > 0),
    source_revision BIGINT NOT NULL
        CONSTRAINT lexicon_entry_publications_revision_check CHECK (source_revision > 0),
    content_schema_version SMALLINT NOT NULL
        CONSTRAINT lexicon_entry_publications_schema_version_check CHECK (content_schema_version = 2),
    snapshot JSONB NOT NULL,
    snapshot_hash BYTEA NOT NULL,
    published_by_admin_id UUID NOT NULL
        CONSTRAINT lexicon_entry_publications_admin_fkey
        REFERENCES admins(id) ON DELETE RESTRICT,
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT lexicon_entry_publications_entry_number_key UNIQUE (entry_id, publication_number),
    CONSTRAINT lexicon_entry_publications_entry_revision_key UNIQUE (entry_id, source_revision),
    CONSTRAINT lexicon_entry_publications_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_entry_publications_entry_hash_key UNIQUE (entry_id, snapshot_hash)
);

CREATE INDEX lexicon_entry_publications_entry_idx
    ON lexicon.entry_publications (entry_id, publication_number DESC);

ALTER TABLE lexicon.entries
    ADD CONSTRAINT lexicon_entries_current_publication_fkey
        FOREIGN KEY (current_publication_id, id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE RESTRICT,
    ADD CONSTRAINT lexicon_entries_draft_publication_fkey
        FOREIGN KEY (draft_based_on_publication_id, id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE RESTRICT;

CREATE TABLE lexicon.entry_publication_nodes (
    publication_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    node_id UUID NOT NULL,
    node_type TEXT NOT NULL
        CONSTRAINT lexicon_entry_publication_nodes_type_check CHECK (node_type IN (
            'pos', 'form_group', 'form_slot', 'form_variant', 'pronunciation',
            'sense_group', 'grammar_structure', 'sense', 'definition', 'sentence',
            'text_variant', 'relation'
        )),
    content_hash BYTEA,
    PRIMARY KEY (publication_id, node_id),
    CONSTRAINT lexicon_entry_publication_nodes_publication_fkey
        FOREIGN KEY (publication_id, entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_entry_publication_nodes_node_fkey
        FOREIGN KEY (node_id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_entry_publication_nodes_publication_entry_node_key
        UNIQUE (publication_id, entry_id, node_id),
    CONSTRAINT lexicon_entry_publication_nodes_hash_key
        UNIQUE (publication_id, node_id, content_hash)
);

CREATE INDEX lexicon_entry_publication_nodes_entry_idx
    ON lexicon.entry_publication_nodes (entry_id, node_type, node_id);

CREATE TABLE lexicon.entry_publication_part_of_speech_refs (
    publication_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    source_node_id UUID NOT NULL,
    part_of_speech_id UUID NOT NULL,
    PRIMARY KEY (publication_id, source_node_id),
    CONSTRAINT lexicon_publication_pos_refs_publication_fkey
        FOREIGN KEY (publication_id, entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_publication_pos_refs_node_fkey
        FOREIGN KEY (source_node_id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_publication_pos_refs_catalog_fkey
        FOREIGN KEY (part_of_speech_id)
        REFERENCES catalog.parts_of_speech(id) ON DELETE RESTRICT
);

CREATE INDEX lexicon_publication_pos_refs_catalog_idx
    ON lexicon.entry_publication_part_of_speech_refs (part_of_speech_id, entry_id);

CREATE TABLE lexicon.entry_publication_sub_part_of_speech_refs (
    publication_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    source_node_id UUID NOT NULL,
    sub_part_of_speech_id UUID NOT NULL,
    PRIMARY KEY (publication_id, source_node_id),
    CONSTRAINT lexicon_publication_sub_pos_refs_publication_fkey
        FOREIGN KEY (publication_id, entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_publication_sub_pos_refs_node_fkey
        FOREIGN KEY (source_node_id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_publication_sub_pos_refs_catalog_fkey
        FOREIGN KEY (sub_part_of_speech_id)
        REFERENCES catalog.sub_parts_of_speech(id) ON DELETE RESTRICT
);

CREATE INDEX lexicon_publication_sub_pos_refs_catalog_idx
    ON lexicon.entry_publication_sub_part_of_speech_refs (sub_part_of_speech_id, source_node_id);

-- 跨词条词义引用必须同时锚定来源发布版本和目标发布版本。
-- 仅记录 relation 与外部 context；focus 和同词条 context 由同一 publication 的节点集合保证。
CREATE TABLE lexicon.entry_publication_sense_refs (
    publication_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    source_node_id UUID NOT NULL,
    reference_kind TEXT NOT NULL
        CONSTRAINT lexicon_publication_sense_refs_kind_check
        CHECK (reference_kind IN ('relation', 'sentence_context')),
    target_entry_id UUID NOT NULL,
    target_sense_id UUID NOT NULL,
    target_publication_id UUID NOT NULL,
    PRIMARY KEY (
        publication_id, source_node_id, reference_kind,
        target_entry_id, target_sense_id
    ),
    CONSTRAINT lexicon_publication_sense_refs_external_check
        CHECK (entry_id <> target_entry_id),
    CONSTRAINT lexicon_publication_sense_refs_source_fkey
        FOREIGN KEY (publication_id, entry_id, source_node_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE CASCADE,
    CONSTRAINT lexicon_publication_sense_refs_target_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_sense_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE
);

CREATE INDEX lexicon_publication_sense_refs_inbound_idx
    ON lexicon.entry_publication_sense_refs (
        target_entry_id, target_sense_id, publication_id
    );
CREATE INDEX lexicon_publication_sense_refs_target_publication_idx
    ON lexicon.entry_publication_sense_refs (
        target_publication_id, target_entry_id, target_sense_id
    );

CREATE TABLE platform.outbox_events (
    id UUID PRIMARY KEY,
    aggregate_type TEXT NOT NULL,
    aggregate_id UUID NOT NULL,
    aggregate_revision BIGINT NOT NULL
        CONSTRAINT platform_outbox_revision_check CHECK (aggregate_revision > 0),
    event_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts INTEGER NOT NULL DEFAULT 0
        CONSTRAINT platform_outbox_attempts_check CHECK (attempts >= 0),
    locked_until TIMESTAMPTZ,
    processed_at TIMESTAMPTZ,
    last_error TEXT,
    CONSTRAINT platform_outbox_event_key
        UNIQUE (aggregate_type, aggregate_id, aggregate_revision, event_type)
);

CREATE INDEX platform_outbox_pending_idx
    ON platform.outbox_events (available_at, occurred_at, id)
    WHERE processed_at IS NULL;

CREATE TABLE audit.admin_actions (
    id UUID PRIMARY KEY,
    actor_admin_id UUID NOT NULL
        CONSTRAINT audit_admin_actions_actor_fkey
        REFERENCES admins(id) ON DELETE RESTRICT,
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id UUID,
    resource_revision BIGINT,
    request_id UUID NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT audit_admin_actions_revision_check
        CHECK (resource_revision IS NULL OR resource_revision > 0)
);

CREATE INDEX audit_admin_actions_resource_idx
    ON audit.admin_actions (resource_type, resource_id, occurred_at DESC);
CREATE INDEX audit_admin_actions_actor_idx
    ON audit.admin_actions (actor_admin_id, occurred_at DESC);
