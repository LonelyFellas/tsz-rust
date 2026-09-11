-- 回退到「V2 与 V3 并存」的库形状。表恢复后必然是空的：V2 内容早已清零，这条回退只为让
-- deployment_migrations 的单事务 undo 能原样走通，不承诺恢复任何数据。
ALTER TABLE lexicon.surface_sources
    DROP CONSTRAINT lexicon_surface_sources_schema_version_check,
    DROP CONSTRAINT lexicon_surface_sources_source_shape_check,
    ALTER COLUMN content_schema_version SET DEFAULT 2,
    ADD CONSTRAINT lexicon_surface_sources_schema_version_check
        CHECK (content_schema_version = ANY (ARRAY[2, 3])),
    ADD CONSTRAINT lexicon_surface_sources_source_shape_check
        CHECK (
            content_schema_version = 2
            AND (source_kind = ANY (ARRAY['headword'::text, 'form'::text]))
            AND form_id IS NULL AND variant_id IS NULL AND group_ids IS NULL
            AND projection_version IS NULL
            AND (
                source_kind = 'headword'::text AND source_node_id IS NULL
                AND pos_id IS NULL AND pos IS NULL AND form_type IS NULL
                OR source_kind = 'form'::text AND source_node_id IS NOT NULL
                AND pos_id IS NOT NULL AND pos IS NOT NULL AND btrim(pos) <> ''::text
                AND form_type IS NOT NULL AND form_type ~ '^[a-z][a-z0-9_]{0,31}$'::text
            )
            OR content_schema_version = 3
            AND source_kind = 'form_variant'::text
            AND source_node_id IS NOT NULL AND source_node_id = variant_id
            AND pos_id IS NOT NULL AND pos IS NOT NULL AND btrim(pos) <> ''::text
            AND form_id IS NOT NULL AND variant_id IS NOT NULL
            AND group_ids IS NOT NULL AND cardinality(group_ids) > 0
            AND array_position(group_ids, NULL::uuid) IS NULL
            AND projection_version IS NOT NULL
            AND projection_version = btrim(projection_version)
            AND char_length(projection_version) >= 1
            AND char_length(projection_version) <= 100
            AND form_type IS NOT NULL AND form_type ~ '^[a-z][a-z0-9_]{0,31}$'::text
            AND (
                dialect = 'common'::text AND (dialect_scope = ANY (ARRAY['uk'::text, 'us'::text]))
                OR dialect = 'uk'::text AND dialect_scope = 'uk'::text
                OR dialect = 'us'::text AND dialect_scope = 'us'::text
            )
        );

ALTER TABLE lexicon.entry_pos
    DROP CONSTRAINT lexicon_entry_pos_schema_version_check,
    DROP CONSTRAINT lexicon_entry_pos_versioned_modes_check,
    ALTER COLUMN content_schema_version SET DEFAULT 2,
    ADD CONSTRAINT lexicon_entry_pos_schema_version_check
        CHECK (content_schema_version = ANY (ARRAY[2, 3])),
    ADD CONSTRAINT lexicon_entry_pos_versioned_modes_check
        CHECK (
            (content_schema_version <> ALL (ARRAY[2, 3]))
            OR (content_schema_version = ANY (ARRAY[2, 3]))
            AND spelling_mode IS NOT NULL
            AND phonetic_mode IS NOT NULL
            AND (spelling_mode <> 'distinguish'::text OR phonetic_mode = 'distinguish'::text)
        );

ALTER TABLE lexicon.v3_entry_state
    DROP CONSTRAINT lexicon_v3_entry_state_origin_check,
    ADD COLUMN migration_batch_id uuid,
    ADD COLUMN source_publication_id uuid,
    ADD COLUMN source_revision bigint,
    ADD COLUMN publication_canary_enabled boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT lexicon_v3_entry_state_origin_check
        CHECK (origin = ANY (ARRAY['native'::text, 'migrated_v2'::text])),
    ADD CONSTRAINT lexicon_v3_entry_state_source_revision_check
        CHECK (source_revision IS NULL OR source_revision > 0),
    ADD CONSTRAINT lexicon_v3_entry_state_origin_shape_check
        CHECK (
            origin = 'native'::text
            AND migration_batch_id IS NULL
            AND source_publication_id IS NULL
            AND source_revision IS NULL
            AND publication_canary_enabled = false
            OR origin = 'migrated_v2'::text
            AND migration_batch_id IS NOT NULL
            AND source_revision IS NOT NULL
            AND (publication_canary_enabled = false OR source_publication_id IS NOT NULL)
        ),
    ADD CONSTRAINT lexicon_v3_entry_state_source_publication_fkey
        FOREIGN KEY (source_publication_id, entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE RESTRICT;

CREATE INDEX lexicon_v3_entry_state_migration_batch_idx
    ON lexicon.v3_entry_state (migration_batch_id, entry_id)
    WHERE migration_batch_id IS NOT NULL;

ALTER TABLE lexicon.entries
    DROP CONSTRAINT lexicon_entries_schema_version_check,
    DROP CONSTRAINT lexicon_entries_schema_kind_check,
    ADD COLUMN headword_mode text,
    ADD COLUMN source_dialect text,
    ALTER COLUMN content_schema_version SET DEFAULT 2,
    ADD CONSTRAINT lexicon_entries_schema_version_check
        CHECK (content_schema_version = ANY (ARRAY[2, 3])),
    ADD CONSTRAINT lexicon_entries_schema_kind_check
        CHECK (content_schema_version <> 3 OR kind = ANY (ARRAY['word'::text, 'phrase'::text])),
    ADD CONSTRAINT lexicon_entries_headword_mode_check
        CHECK (headword_mode = ANY (ARRAY['unified'::text, 'distinguish'::text])),
    ADD CONSTRAINT lexicon_entries_source_dialect_check
        CHECK (source_dialect = ANY (ARRAY['uk'::text, 'us'::text])),
    ADD CONSTRAINT lexicon_entries_versioned_headword_shape_check
        CHECK (
            content_schema_version <> ALL (ARRAY[2, 3])
            OR content_schema_version = 2
            AND headword_mode IS NOT NULL
            AND (
                headword_mode = 'unified'::text AND source_dialect IS NULL
                OR headword_mode = 'distinguish'::text AND source_dialect IS NOT NULL
            )
            OR content_schema_version = 3
            AND (
                headword_mode IS NULL AND source_dialect IS NULL
                OR headword_mode = 'unified'::text AND source_dialect IS NULL
                OR headword_mode = 'distinguish'::text AND source_dialect IS NOT NULL
            )
        );

CREATE TABLE lexicon.entry_headword_keys (
    entry_id uuid NOT NULL,
    language text NOT NULL,
    kind text NOT NULL,
    dialect_scope text NOT NULL,
    normalized_headword text NOT NULL,
    normalization_version smallint NOT NULL,
    CONSTRAINT lexicon_entry_headword_keys_dialect_check CHECK ((dialect_scope = ANY (ARRAY['uk'::text, 'us'::text]))),
    CONSTRAINT lexicon_entry_headword_keys_kind_check CHECK ((kind = ANY (ARRAY['word'::text, 'phrase'::text]))),
    CONSTRAINT lexicon_entry_headword_keys_language_check CHECK ((language = 'en'::text)),
    CONSTRAINT lexicon_entry_headword_keys_nonempty_check CHECK (((normalized_headword = btrim(normalized_headword)) AND ((char_length(normalized_headword) >= 1) AND (char_length(normalized_headword) <= 200))))
);
CREATE TABLE lexicon.entry_headwords (
    id uuid NOT NULL,
    entry_id uuid NOT NULL,
    dialect text NOT NULL,
    headword text NOT NULL,
    normalized_headword text NOT NULL,
    normalization_version smallint NOT NULL,
    origin text NOT NULL,
    CONSTRAINT lexicon_entry_headwords_dialect_check CHECK ((dialect = ANY (ARRAY['common'::text, 'uk'::text, 'us'::text]))),
    CONSTRAINT lexicon_entry_headwords_nonempty_check CHECK (((headword = btrim(headword)) AND (normalized_headword = btrim(normalized_headword)) AND ((char_length(headword) >= 1) AND (char_length(headword) <= 200)) AND ((char_length(normalized_headword) >= 1) AND (char_length(normalized_headword) <= 200)))),
    CONSTRAINT lexicon_entry_headwords_origin_check CHECK ((origin = ANY (ARRAY['dictionary'::text, 'converted'::text, 'manual'::text])))
);
CREATE TABLE lexicon.form_groups (
    id uuid NOT NULL,
    entry_id uuid NOT NULL,
    entry_pos_id uuid NOT NULL,
    is_regular boolean NOT NULL,
    sort_order integer NOT NULL,
    CONSTRAINT lexicon_form_groups_sort_order_check CHECK ((sort_order >= 0))
);
CREATE TABLE lexicon.form_slots (
    id uuid NOT NULL,
    entry_id uuid NOT NULL,
    entry_pos_id uuid NOT NULL,
    form_group_id uuid,
    form_type text NOT NULL,
    sort_order integer NOT NULL,
    CONSTRAINT lexicon_form_slots_group_shape_check CHECK ((((form_type = 'base'::text) AND (form_group_id IS NULL)) OR ((form_type <> 'base'::text) AND (form_group_id IS NOT NULL)))),
    CONSTRAINT lexicon_form_slots_sort_order_check CHECK ((sort_order >= 0)),
    CONSTRAINT lexicon_form_slots_type_check CHECK ((form_type ~ '^[a-z][a-z0-9_]{0,31}$'::text))
);
CREATE TABLE lexicon.form_variants (
    id uuid NOT NULL,
    entry_id uuid NOT NULL,
    form_slot_id uuid NOT NULL,
    dialect text NOT NULL,
    spelling text NOT NULL,
    origin text NOT NULL,
    sort_order integer NOT NULL,
    CONSTRAINT lexicon_form_variants_dialect_check CHECK ((dialect = ANY (ARRAY['common'::text, 'uk'::text, 'us'::text]))),
    CONSTRAINT lexicon_form_variants_origin_check CHECK ((origin = ANY (ARRAY['dictionary'::text, 'converted'::text, 'manual'::text]))),
    CONSTRAINT lexicon_form_variants_sort_order_check CHECK ((sort_order >= 0)),
    CONSTRAINT lexicon_form_variants_spelling_check CHECK (((spelling = btrim(spelling)) AND (char_length(spelling) <= 200)))
);
CREATE TABLE lexicon.pronunciations (
    id uuid NOT NULL,
    entry_id uuid NOT NULL,
    form_variant_id uuid NOT NULL,
    dict_phonetic text NOT NULL,
    actual_pron text NOT NULL,
    style text NOT NULL,
    sort_order integer NOT NULL,
    CONSTRAINT lexicon_pronunciations_lengths_check CHECK (((char_length(dict_phonetic) <= 200) AND (char_length(actual_pron) <= 200))),
    CONSTRAINT lexicon_pronunciations_sort_order_check CHECK ((sort_order >= 0)),
    CONSTRAINT lexicon_pronunciations_style_check CHECK ((style = ANY (ARRAY['normal'::text, 'strong'::text, 'weak'::text])))
);
CREATE TABLE lexicon.v3_migration_batches (
    id uuid NOT NULL,
    source_schema_version smallint DEFAULT 2 NOT NULL,
    target_schema_version smallint DEFAULT 3 NOT NULL,
    status text NOT NULL,
    selection_digest bytea NOT NULL,
    manifest_digest bytea NOT NULL,
    requested_by_admin_id uuid NOT NULL,
    request_id uuid NOT NULL,
    approved_by_admin_id uuid,
    approval_request_id uuid,
    approved_at timestamp with time zone,
    scanned_count integer DEFAULT 0 NOT NULL,
    eligible_count integer DEFAULT 0 NOT NULL,
    applied_count integer DEFAULT 0 NOT NULL,
    blocked_count integer DEFAULT 0 NOT NULL,
    failed_count integer DEFAULT 0 NOT NULL,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    finished_at timestamp with time zone,
    CONSTRAINT lexicon_v3_migration_batches_applied_check CHECK ((applied_count >= 0)),
    CONSTRAINT lexicon_v3_migration_batches_approval_shape_check CHECK ((((status = 'planned'::text) AND (approved_by_admin_id IS NULL) AND (approval_request_id IS NULL) AND (approved_at IS NULL)) OR ((status <> 'planned'::text) AND (approved_by_admin_id IS NOT NULL) AND (approval_request_id IS NOT NULL) AND (approved_at IS NOT NULL)))),
    CONSTRAINT lexicon_v3_migration_batches_blocked_check CHECK ((blocked_count >= 0)),
    CONSTRAINT lexicon_v3_migration_batches_eligible_check CHECK ((eligible_count >= 0)),
    CONSTRAINT lexicon_v3_migration_batches_failed_check CHECK ((failed_count >= 0)),
    CONSTRAINT lexicon_v3_migration_batches_scanned_check CHECK ((scanned_count >= 0)),
    CONSTRAINT lexicon_v3_migration_batches_source_version_check CHECK ((source_schema_version = 2)),
    CONSTRAINT lexicon_v3_migration_batches_status_check CHECK ((status = ANY (ARRAY['planned'::text, 'approved'::text, 'applying'::text, 'applied'::text, 'verified'::text, 'rolled_back'::text, 'failed'::text]))),
    CONSTRAINT lexicon_v3_migration_batches_target_version_check CHECK ((target_schema_version = 3))
);
CREATE TABLE lexicon.v3_migration_entries (
    batch_id uuid NOT NULL,
    entry_id uuid NOT NULL,
    status text NOT NULL,
    source_revision bigint,
    source_current_publication_id uuid,
    source_publications_digest bytea,
    source_pos_modes jsonb,
    source_forms jsonb,
    source_meanings jsonb,
    source_draft_surfaces jsonb,
    expected_forms jsonb,
    expected_presentation jsonb,
    expected_digest bytea,
    applied_digest bytea,
    block_code text,
    failure_code text,
    applied_at timestamp with time zone,
    verified_at timestamp with time zone,
    rolled_back_at timestamp with time zone,
    CONSTRAINT lexicon_v3_migration_entries_payload_shape_check CHECK ((((status = ANY (ARRAY['planned'::text, 'applied'::text, 'verified'::text, 'rolled_back'::text])) AND (source_revision IS NOT NULL) AND (source_publications_digest IS NOT NULL) AND (source_pos_modes IS NOT NULL) AND (source_forms IS NOT NULL) AND (source_meanings IS NOT NULL) AND (source_draft_surfaces IS NOT NULL) AND (expected_forms IS NOT NULL) AND (expected_presentation IS NOT NULL) AND (expected_digest IS NOT NULL) AND (block_code IS NULL) AND (failure_code IS NULL)) OR ((status = 'blocked'::text) AND (block_code IS NOT NULL) AND (applied_digest IS NULL) AND (failure_code IS NULL)) OR ((status = 'failed'::text) AND (failure_code IS NOT NULL)))),
    CONSTRAINT lexicon_v3_migration_entries_revision_check CHECK (((source_revision IS NULL) OR (source_revision > 0))),
    CONSTRAINT lexicon_v3_migration_entries_status_check CHECK ((status = ANY (ARRAY['planned'::text, 'applied'::text, 'verified'::text, 'rolled_back'::text, 'blocked'::text, 'failed'::text])))
);
CREATE TABLE lexicon.v3_migration_map (
    batch_id uuid NOT NULL,
    entry_id uuid NOT NULL,
    v2_node_id uuid,
    v3_node_id uuid NOT NULL,
    role text NOT NULL,
    mapping_kind text NOT NULL,
    CONSTRAINT lexicon_v3_migration_map_kind_check CHECK ((mapping_kind = ANY (ARRAY['preserved'::text, 'deterministic_generated'::text]))),
    CONSTRAINT lexicon_v3_migration_map_role_check CHECK ((role = ANY (ARRAY['entry'::text, 'pos'::text, 'form_group'::text, 'synthetic_base_only_group'::text, 'concrete_form'::text, 'group_membership'::text, 'form_variant'::text, 'pronunciation'::text])))
);
ALTER TABLE ONLY lexicon.entry_headword_keys
    ADD CONSTRAINT entry_headword_keys_pkey PRIMARY KEY (entry_id, dialect_scope);
ALTER TABLE ONLY lexicon.entry_headwords
    ADD CONSTRAINT entry_headwords_pkey PRIMARY KEY (id);
ALTER TABLE ONLY lexicon.form_groups
    ADD CONSTRAINT form_groups_pkey PRIMARY KEY (id);
ALTER TABLE ONLY lexicon.form_slots
    ADD CONSTRAINT form_slots_pkey PRIMARY KEY (id);
ALTER TABLE ONLY lexicon.form_variants
    ADD CONSTRAINT form_variants_pkey PRIMARY KEY (id);
ALTER TABLE ONLY lexicon.entry_headwords
    ADD CONSTRAINT lexicon_entry_headwords_entry_dialect_key UNIQUE (entry_id, dialect);
ALTER TABLE ONLY lexicon.form_groups
    ADD CONSTRAINT lexicon_form_groups_id_entry_key UNIQUE (id, entry_id);
ALTER TABLE ONLY lexicon.form_slots
    ADD CONSTRAINT lexicon_form_slots_id_entry_key UNIQUE (id, entry_id);
ALTER TABLE ONLY lexicon.form_variants
    ADD CONSTRAINT lexicon_form_variants_id_entry_key UNIQUE (id, entry_id);
ALTER TABLE ONLY lexicon.form_variants
    ADD CONSTRAINT lexicon_form_variants_slot_dialect_key UNIQUE (form_slot_id, dialect);
ALTER TABLE ONLY lexicon.pronunciations
    ADD CONSTRAINT lexicon_pronunciations_id_entry_key UNIQUE (id, entry_id);
ALTER TABLE ONLY lexicon.v3_migration_map
    ADD CONSTRAINT lexicon_v3_migration_map_v3_node_entry_key UNIQUE (batch_id, entry_id, v3_node_id);
ALTER TABLE ONLY lexicon.pronunciations
    ADD CONSTRAINT pronunciations_pkey PRIMARY KEY (id);
ALTER TABLE ONLY lexicon.v3_migration_batches
    ADD CONSTRAINT v3_migration_batches_pkey PRIMARY KEY (id);
ALTER TABLE ONLY lexicon.v3_migration_entries
    ADD CONSTRAINT v3_migration_entries_pkey PRIMARY KEY (batch_id, entry_id);
ALTER TABLE ONLY lexicon.v3_migration_map
    ADD CONSTRAINT v3_migration_map_pkey PRIMARY KEY (batch_id, entry_id, role, v3_node_id);
CREATE INDEX form_slots_form_type_idx ON lexicon.form_slots USING btree (form_type, entry_id);
CREATE INDEX lexicon_entry_headword_keys_lookup_idx ON lexicon.entry_headword_keys USING btree (language, dialect_scope, normalized_headword, kind, entry_id);
CREATE INDEX lexicon_entry_headwords_entry_idx ON lexicon.entry_headwords USING btree (entry_id);
CREATE INDEX lexicon_form_groups_entry_idx ON lexicon.form_groups USING btree (entry_id, entry_pos_id, sort_order, id);
CREATE INDEX lexicon_form_slots_entry_idx ON lexicon.form_slots USING btree (entry_id, entry_pos_id, form_group_id, sort_order, id);
CREATE UNIQUE INDEX lexicon_form_slots_one_base_idx ON lexicon.form_slots USING btree (entry_pos_id) WHERE (form_type = 'base'::text);
CREATE INDEX lexicon_form_variants_entry_idx ON lexicon.form_variants USING btree (entry_id, form_slot_id, sort_order, id);
CREATE INDEX lexicon_pronunciations_entry_idx ON lexicon.pronunciations USING btree (entry_id, form_variant_id, sort_order, id);
CREATE INDEX lexicon_v3_migration_batches_status_idx ON lexicon.v3_migration_batches USING btree (status, started_at, id);
CREATE INDEX lexicon_v3_migration_entries_batch_status_idx ON lexicon.v3_migration_entries USING btree (batch_id, status, entry_id);
CREATE UNIQUE INDEX lexicon_v3_migration_entries_live_entry_key ON lexicon.v3_migration_entries USING btree (entry_id) WHERE (status = ANY (ARRAY['planned'::text, 'applied'::text, 'verified'::text]));
CREATE INDEX lexicon_v3_migration_map_v2_idx ON lexicon.v3_migration_map USING btree (entry_id, v2_node_id, role) WHERE (v2_node_id IS NOT NULL);
ALTER TABLE ONLY lexicon.form_slots
    ADD CONSTRAINT form_slots_form_type_catalog_fkey FOREIGN KEY (form_type) REFERENCES catalog.form_types(code) ON DELETE RESTRICT;
ALTER TABLE ONLY lexicon.entry_headword_keys
    ADD CONSTRAINT lexicon_entry_headword_keys_entry_fkey FOREIGN KEY (entry_id) REFERENCES lexicon.entries(id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.entry_headwords
    ADD CONSTRAINT lexicon_entry_headwords_entry_fkey FOREIGN KEY (entry_id) REFERENCES lexicon.entries(id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.form_groups
    ADD CONSTRAINT lexicon_form_groups_node_fkey FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.form_groups
    ADD CONSTRAINT lexicon_form_groups_pos_fkey FOREIGN KEY (entry_pos_id, entry_id) REFERENCES lexicon.entry_pos(id, entry_id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.form_slots
    ADD CONSTRAINT lexicon_form_slots_group_fkey FOREIGN KEY (form_group_id, entry_id) REFERENCES lexicon.form_groups(id, entry_id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.form_slots
    ADD CONSTRAINT lexicon_form_slots_node_fkey FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.form_slots
    ADD CONSTRAINT lexicon_form_slots_pos_fkey FOREIGN KEY (entry_pos_id, entry_id) REFERENCES lexicon.entry_pos(id, entry_id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.form_variants
    ADD CONSTRAINT lexicon_form_variants_node_fkey FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.form_variants
    ADD CONSTRAINT lexicon_form_variants_slot_fkey FOREIGN KEY (form_slot_id, entry_id) REFERENCES lexicon.form_slots(id, entry_id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.pronunciations
    ADD CONSTRAINT lexicon_pronunciations_node_fkey FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.pronunciations
    ADD CONSTRAINT lexicon_pronunciations_variant_fkey FOREIGN KEY (form_variant_id, entry_id) REFERENCES lexicon.form_variants(id, entry_id) ON DELETE CASCADE;
ALTER TABLE ONLY lexicon.v3_migration_batches
    ADD CONSTRAINT lexicon_v3_migration_batches_admin_fkey FOREIGN KEY (requested_by_admin_id) REFERENCES public.admins(id) ON DELETE RESTRICT;
ALTER TABLE ONLY lexicon.v3_migration_batches
    ADD CONSTRAINT lexicon_v3_migration_batches_approved_admin_fkey FOREIGN KEY (approved_by_admin_id) REFERENCES public.admins(id) ON DELETE RESTRICT;
ALTER TABLE ONLY lexicon.v3_migration_entries
    ADD CONSTRAINT lexicon_v3_migration_entries_batch_fkey FOREIGN KEY (batch_id) REFERENCES lexicon.v3_migration_batches(id) ON DELETE RESTRICT;
ALTER TABLE ONLY lexicon.v3_migration_entries
    ADD CONSTRAINT lexicon_v3_migration_entries_entry_fkey FOREIGN KEY (entry_id) REFERENCES lexicon.entries(id) ON DELETE RESTRICT;
ALTER TABLE ONLY lexicon.v3_migration_entries
    ADD CONSTRAINT lexicon_v3_migration_entries_publication_fkey FOREIGN KEY (source_current_publication_id, entry_id) REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE RESTRICT;
ALTER TABLE ONLY lexicon.v3_migration_map
    ADD CONSTRAINT lexicon_v3_migration_map_entry_fkey FOREIGN KEY (batch_id, entry_id) REFERENCES lexicon.v3_migration_entries(batch_id, entry_id) ON DELETE CASCADE;
