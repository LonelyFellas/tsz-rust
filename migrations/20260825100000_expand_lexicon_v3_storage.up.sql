-- Smart Lexicon V3 expand-only canonical storage.
--
-- V2 tables and rows remain authoritative for schema version 2. V3 uses
-- separate form tables because V2 form_slots cannot represent peer forms,
-- repeated form_type values, or one form belonging to more than one group.

ALTER TABLE lexicon.entries
    DROP CONSTRAINT lexicon_entries_schema_version_check,
    DROP CONSTRAINT lexicon_entries_headword_shape_check,
    ALTER COLUMN headword_mode DROP NOT NULL;

ALTER TABLE lexicon.entries
    ADD CONSTRAINT lexicon_entries_schema_version_check
        CHECK (content_schema_version IN (2, 3)),
    ADD CONSTRAINT lexicon_entries_schema_kind_check
        CHECK (content_schema_version <> 3 OR kind = 'word'),
    ADD CONSTRAINT lexicon_entries_versioned_headword_shape_check CHECK (
        content_schema_version NOT IN (2, 3)
        OR (
            content_schema_version = 2
            AND headword_mode IS NOT NULL
            AND (
                (headword_mode = 'unified' AND source_dialect IS NULL)
                OR (headword_mode = 'distinguish' AND source_dialect IS NOT NULL)
            )
        )
        OR (
            content_schema_version = 3
            AND (
                (headword_mode IS NULL AND source_dialect IS NULL)
                OR (headword_mode = 'unified' AND source_dialect IS NULL)
                OR (headword_mode = 'distinguish' AND source_dialect IS NOT NULL)
            )
        )
    ),
    ADD CONSTRAINT lexicon_entries_id_schema_version_key
        UNIQUE (id, content_schema_version);

-- A V3 POS is still the existing stable POS node, but it no longer owns the
-- V2 spelling/phonetic modes. The version column keeps old writers on V2 by
-- default and makes a V3 POS/entry mismatch impossible.
ALTER TABLE lexicon.entry_pos
    ADD COLUMN content_schema_version SMALLINT NOT NULL DEFAULT 2,
    ALTER COLUMN spelling_mode DROP NOT NULL,
    ALTER COLUMN phonetic_mode DROP NOT NULL,
    DROP CONSTRAINT lexicon_entry_pos_modes_check;

ALTER TABLE lexicon.entry_pos
    ADD CONSTRAINT lexicon_entry_pos_schema_version_check
        CHECK (content_schema_version IN (2, 3)),
    ADD CONSTRAINT lexicon_entry_pos_versioned_modes_check CHECK (
        content_schema_version NOT IN (2, 3)
        OR (
            content_schema_version = 2
            AND spelling_mode IS NOT NULL
            AND phonetic_mode IS NOT NULL
            AND (spelling_mode <> 'distinguish' OR phonetic_mode = 'distinguish')
        )
        OR (
            content_schema_version = 3
            AND spelling_mode IS NULL
            AND phonetic_mode IS NULL
        )
    ),
    ADD CONSTRAINT lexicon_entry_pos_id_entry_schema_key
        UNIQUE (id, entry_id, content_schema_version),
    ADD CONSTRAINT lexicon_entry_pos_entry_schema_fkey
        FOREIGN KEY (entry_id, content_schema_version)
        REFERENCES lexicon.entries(id, content_schema_version)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED;

CREATE UNIQUE INDEX lexicon_entry_pos_v3_ordinal_key
    ON lexicon.entry_pos (entry_id, sort_order)
    WHERE content_schema_version = 3;

ALTER TABLE lexicon.nodes
    DROP CONSTRAINT lexicon_nodes_type_check,
    ADD CONSTRAINT lexicon_nodes_type_check CHECK (node_type IN (
        'pos', 'form_group', 'form_slot', 'concrete_form', 'group_membership',
        'form_variant', 'pronunciation', 'sense_group', 'grammar_structure',
        'sense', 'definition', 'sentence', 'text_variant', 'relation'
    ));

-- Publication snapshots are append-only. This only admits self-describing V3
-- snapshots; it does not rewrite any V2 snapshot or activate V3 publication.
ALTER TABLE lexicon.entry_publications
    DROP CONSTRAINT lexicon_entry_publications_schema_version_check,
    DROP CONSTRAINT lexicon_entry_publications_entry_revision_key,
    ADD CONSTRAINT lexicon_entry_publications_schema_version_check
        CHECK (content_schema_version IN (2, 3)),
    ADD CONSTRAINT lexicon_entry_publications_entry_schema_revision_key
        UNIQUE (entry_id, content_schema_version, source_revision);

ALTER TABLE lexicon.entry_publication_nodes
    DROP CONSTRAINT lexicon_entry_publication_nodes_type_check,
    ADD CONSTRAINT lexicon_entry_publication_nodes_type_check CHECK (node_type IN (
        'pos', 'form_group', 'form_slot', 'concrete_form', 'group_membership',
        'form_variant', 'pronunciation', 'sense_group', 'grammar_structure',
        'sense', 'definition', 'sentence', 'text_variant', 'relation'
    ));

CREATE TABLE lexicon.v3_entry_state (
    entry_id UUID PRIMARY KEY,
    content_schema_version SMALLINT NOT NULL DEFAULT 3
        CONSTRAINT lexicon_v3_entry_state_schema_version_check
        CHECK (content_schema_version = 3),
    origin TEXT NOT NULL
        CONSTRAINT lexicon_v3_entry_state_origin_check
        CHECK (origin IN ('native', 'migrated_v2')),
    migration_batch_id UUID,
    source_publication_id UUID,
    source_revision BIGINT
        CONSTRAINT lexicon_v3_entry_state_source_revision_check
        CHECK (source_revision IS NULL OR source_revision > 0),
    first_v3_write_revision BIGINT
        CONSTRAINT lexicon_v3_entry_state_first_write_revision_check
        CHECK (first_v3_write_revision IS NULL OR first_v3_write_revision > 0),
    publication_canary_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT lexicon_v3_entry_state_entry_fkey
        FOREIGN KEY (entry_id, content_schema_version)
        REFERENCES lexicon.entries(id, content_schema_version)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT lexicon_v3_entry_state_source_publication_fkey
        FOREIGN KEY (source_publication_id, entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id)
        ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_entry_state_origin_shape_check CHECK (
        (
            origin = 'native'
            AND migration_batch_id IS NULL
            AND source_publication_id IS NULL
            AND source_revision IS NULL
            AND publication_canary_enabled = FALSE
        )
        OR (
            origin = 'migrated_v2'
            AND migration_batch_id IS NOT NULL
            AND source_revision IS NOT NULL
            AND (
                publication_canary_enabled = FALSE
                OR source_publication_id IS NOT NULL
            )
        )
    )
);

CREATE INDEX lexicon_v3_entry_state_migration_batch_idx
    ON lexicon.v3_entry_state (migration_batch_id, entry_id)
    WHERE migration_batch_id IS NOT NULL;

CREATE TABLE lexicon.v3_form_groups (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    entry_pos_id UUID NOT NULL,
    content_schema_version SMALLINT NOT NULL DEFAULT 3
        CONSTRAINT lexicon_v3_form_groups_schema_version_check
        CHECK (content_schema_version = 3),
    is_regular BOOLEAN NOT NULL,
    ordinal INTEGER NOT NULL
        CONSTRAINT lexicon_v3_form_groups_ordinal_check CHECK (ordinal >= 0),
    CONSTRAINT lexicon_v3_form_groups_entry_fkey
        FOREIGN KEY (entry_id)
        REFERENCES lexicon.v3_entry_state(entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_form_groups_node_fkey
        FOREIGN KEY (id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_form_groups_pos_fkey
        FOREIGN KEY (entry_pos_id, entry_id, content_schema_version)
        REFERENCES lexicon.entry_pos(id, entry_id, content_schema_version)
        ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_form_groups_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_v3_form_groups_owner_key
        UNIQUE (id, entry_pos_id, entry_id),
    CONSTRAINT lexicon_v3_form_groups_ordinal_key
        UNIQUE (entry_pos_id, ordinal)
);

CREATE INDEX lexicon_v3_form_groups_entry_idx
    ON lexicon.v3_form_groups (entry_id, entry_pos_id, ordinal, id);

CREATE TABLE lexicon.v3_concrete_forms (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    entry_pos_id UUID NOT NULL,
    content_schema_version SMALLINT NOT NULL DEFAULT 3
        CONSTRAINT lexicon_v3_concrete_forms_schema_version_check
        CHECK (content_schema_version = 3),
    form_type TEXT NOT NULL
        CONSTRAINT lexicon_v3_concrete_forms_type_check CHECK (form_type IN (
            'base', 'present_participle', 'past_tense', 'past_participle',
            'third_person_singular', 'plural', 'comparative', 'superlative'
        )),
    ordinal INTEGER NOT NULL
        CONSTRAINT lexicon_v3_concrete_forms_ordinal_check CHECK (ordinal >= 0),
    CONSTRAINT lexicon_v3_concrete_forms_entry_fkey
        FOREIGN KEY (entry_id)
        REFERENCES lexicon.v3_entry_state(entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_concrete_forms_node_fkey
        FOREIGN KEY (id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_concrete_forms_pos_fkey
        FOREIGN KEY (entry_pos_id, entry_id, content_schema_version)
        REFERENCES lexicon.entry_pos(id, entry_id, content_schema_version)
        ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_concrete_forms_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_v3_concrete_forms_owner_key
        UNIQUE (id, entry_pos_id, entry_id),
    CONSTRAINT lexicon_v3_concrete_forms_ordinal_key
        UNIQUE (entry_pos_id, ordinal)
);

CREATE INDEX lexicon_v3_concrete_forms_entry_idx
    ON lexicon.v3_concrete_forms (entry_id, entry_pos_id, ordinal, id);
CREATE INDEX lexicon_v3_concrete_forms_type_idx
    ON lexicon.v3_concrete_forms (entry_id, entry_pos_id, form_type, id);

CREATE TABLE lexicon.v3_group_memberships (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    entry_pos_id UUID NOT NULL,
    form_group_id UUID NOT NULL,
    form_id UUID NOT NULL,
    ordinal INTEGER NOT NULL
        CONSTRAINT lexicon_v3_group_memberships_ordinal_check CHECK (ordinal >= 0),
    CONSTRAINT lexicon_v3_group_memberships_entry_fkey
        FOREIGN KEY (entry_id)
        REFERENCES lexicon.v3_entry_state(entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_group_memberships_node_fkey
        FOREIGN KEY (id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_group_memberships_group_owner_fkey
        FOREIGN KEY (form_group_id, entry_pos_id, entry_id)
        REFERENCES lexicon.v3_form_groups(id, entry_pos_id, entry_id)
        ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_group_memberships_form_owner_fkey
        FOREIGN KEY (form_id, entry_pos_id, entry_id)
        REFERENCES lexicon.v3_concrete_forms(id, entry_pos_id, entry_id)
        ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_group_memberships_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_v3_group_memberships_group_form_key
        UNIQUE (form_group_id, form_id),
    CONSTRAINT lexicon_v3_group_memberships_ordinal_key
        UNIQUE (form_group_id, ordinal)
);

CREATE INDEX lexicon_v3_group_memberships_form_idx
    ON lexicon.v3_group_memberships (form_id, form_group_id, ordinal, id);
CREATE INDEX lexicon_v3_group_memberships_entry_idx
    ON lexicon.v3_group_memberships (entry_id, entry_pos_id, form_group_id, ordinal, id);

CREATE TABLE lexicon.v3_form_variants (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    form_id UUID NOT NULL,
    dialect TEXT NOT NULL
        CONSTRAINT lexicon_v3_form_variants_dialect_check
        CHECK (dialect IN ('common', 'uk', 'us')),
    spelling TEXT NOT NULL,
    normalized_spelling TEXT NOT NULL,
    normalization_version SMALLINT NOT NULL
        CONSTRAINT lexicon_v3_form_variants_normalization_version_check
        CHECK (normalization_version > 0),
    origin TEXT NOT NULL
        CONSTRAINT lexicon_v3_form_variants_origin_check
        CHECK (origin IN ('dictionary', 'converted', 'manual')),
    CONSTRAINT lexicon_v3_form_variants_entry_fkey
        FOREIGN KEY (entry_id)
        REFERENCES lexicon.v3_entry_state(entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_form_variants_node_fkey
        FOREIGN KEY (id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_form_variants_form_fkey
        FOREIGN KEY (form_id, entry_id)
        REFERENCES lexicon.v3_concrete_forms(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_form_variants_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_v3_form_variants_form_dialect_key UNIQUE (form_id, dialect),
    CONSTRAINT lexicon_v3_form_variants_spelling_check CHECK (
        spelling = btrim(spelling)
        AND normalized_spelling = btrim(normalized_spelling)
        AND char_length(spelling) <= 200
        AND char_length(normalized_spelling) <= 200
    )
);

-- Deliberately non-unique. A spelling can identify several forms or entries;
-- ambiguity remains a detection/search policy concern, not a storage collision.
CREATE INDEX lexicon_v3_form_variants_surface_idx
    ON lexicon.v3_form_variants (
        normalized_spelling, dialect, normalization_version, entry_id, form_id
    );

CREATE TABLE lexicon.v3_pronunciations (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    form_variant_id UUID NOT NULL,
    dict_phonetic TEXT NOT NULL,
    actual_pron TEXT NOT NULL,
    normalized_dict_phonetic TEXT NOT NULL,
    normalized_actual_pron TEXT NOT NULL,
    style TEXT
        CONSTRAINT lexicon_v3_pronunciations_style_check
        CHECK (style IN ('normal', 'strong', 'weak')),
    normalization_version SMALLINT NOT NULL
        CONSTRAINT lexicon_v3_pronunciations_normalization_version_check
        CHECK (normalization_version > 0),
    ordinal INTEGER NOT NULL
        CONSTRAINT lexicon_v3_pronunciations_ordinal_check CHECK (ordinal >= 0),
    CONSTRAINT lexicon_v3_pronunciations_entry_fkey
        FOREIGN KEY (entry_id)
        REFERENCES lexicon.v3_entry_state(entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_pronunciations_node_fkey
        FOREIGN KEY (id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_pronunciations_variant_fkey
        FOREIGN KEY (form_variant_id, entry_id)
        REFERENCES lexicon.v3_form_variants(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_pronunciations_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_v3_pronunciations_ordinal_key
        UNIQUE (form_variant_id, ordinal),
    CONSTRAINT lexicon_v3_pronunciations_lengths_check CHECK (
        char_length(dict_phonetic) <= 200
        AND char_length(actual_pron) <= 200
        AND char_length(normalized_dict_phonetic) <= 200
        AND char_length(normalized_actual_pron) <= 200
        AND normalized_dict_phonetic = btrim(normalized_dict_phonetic)
        AND normalized_actual_pron = btrim(normalized_actual_pron)
    )
);

CREATE INDEX lexicon_v3_pronunciations_entry_idx
    ON lexicon.v3_pronunciations (entry_id, form_variant_id, ordinal, id);

-- Draft rows may omit style and pronunciation text. Once all three normalized
-- identity fields are present, a second identical pronunciation is rejected.
CREATE UNIQUE INDEX lexicon_v3_pronunciations_complete_triple_key
    ON lexicon.v3_pronunciations (
        form_variant_id,
        normalized_dict_phonetic,
        normalized_actual_pron,
        style,
        normalization_version
    )
    WHERE style IS NOT NULL
      AND normalized_dict_phonetic <> ''
      AND normalized_actual_pron <> '';

-- Cross-row integrity is checked at commit so a save transaction can reorder,
-- change common <-> uk/us, or delete the final membership together with its form.
CREATE FUNCTION lexicon.v3_assert_form_has_membership(target_form_id UUID)
RETURNS VOID
LANGUAGE plpgsql
AS $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM lexicon.v3_concrete_forms WHERE id = target_form_id
    ) AND NOT EXISTS (
        SELECT 1 FROM lexicon.v3_group_memberships WHERE form_id = target_form_id
    ) THEN
        RAISE EXCEPTION 'V3 concrete form % has no group membership', target_form_id
            USING ERRCODE = '23514',
                  CONSTRAINT = 'lexicon_v3_concrete_forms_membership_required_check';
    END IF;
END
$$;

CREATE FUNCTION lexicon.v3_check_concrete_form_membership_trigger()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM lexicon.v3_assert_form_has_membership(OLD.id);
    ELSE
        PERFORM lexicon.v3_assert_form_has_membership(NEW.id);
    END IF;
    RETURN NULL;
END
$$;

CREATE FUNCTION lexicon.v3_check_membership_form_trigger()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        PERFORM lexicon.v3_assert_form_has_membership(OLD.form_id);
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        PERFORM lexicon.v3_assert_form_has_membership(NEW.form_id);
    END IF;
    RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER lexicon_v3_concrete_forms_membership_required_trigger
AFTER INSERT OR UPDATE OR DELETE ON lexicon.v3_concrete_forms
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lexicon.v3_check_concrete_form_membership_trigger();

CREATE CONSTRAINT TRIGGER lexicon_v3_group_memberships_form_required_trigger
AFTER INSERT OR UPDATE OR DELETE ON lexicon.v3_group_memberships
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lexicon.v3_check_membership_form_trigger();

CREATE FUNCTION lexicon.v3_assert_form_has_regional_shape(target_form_id UUID)
RETURNS VOID
LANGUAGE plpgsql
AS $$
DECLARE
    variant_count INTEGER;
    common_count INTEGER;
    uk_count INTEGER;
    us_count INTEGER;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM lexicon.v3_concrete_forms WHERE id = target_form_id
    ) THEN
        RETURN;
    END IF;

    SELECT
        count(*)::INTEGER,
        count(*) FILTER (WHERE dialect = 'common')::INTEGER,
        count(*) FILTER (WHERE dialect = 'uk')::INTEGER,
        count(*) FILTER (WHERE dialect = 'us')::INTEGER
    INTO variant_count, common_count, uk_count, us_count
    FROM lexicon.v3_form_variants
    WHERE form_id = target_form_id;

    IF NOT (
        (variant_count = 1 AND common_count = 1)
        OR (variant_count = 2 AND uk_count = 1 AND us_count = 1)
    ) THEN
        RAISE EXCEPTION 'V3 concrete form % must have common xor complete uk/us variants', target_form_id
            USING ERRCODE = '23514',
                  CONSTRAINT = 'lexicon_v3_form_variants_regional_shape_check';
    END IF;
END
$$;

CREATE FUNCTION lexicon.v3_check_concrete_form_regional_trigger()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM lexicon.v3_assert_form_has_regional_shape(OLD.id);
    ELSE
        PERFORM lexicon.v3_assert_form_has_regional_shape(NEW.id);
    END IF;
    RETURN NULL;
END
$$;

CREATE FUNCTION lexicon.v3_check_variant_form_trigger()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        PERFORM lexicon.v3_assert_form_has_regional_shape(OLD.form_id);
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        PERFORM lexicon.v3_assert_form_has_regional_shape(NEW.form_id);
    END IF;
    RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER lexicon_v3_concrete_forms_regional_shape_trigger
AFTER INSERT OR UPDATE OR DELETE ON lexicon.v3_concrete_forms
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lexicon.v3_check_concrete_form_regional_trigger();

CREATE CONSTRAINT TRIGGER lexicon_v3_form_variants_regional_shape_trigger
AFTER INSERT OR UPDATE OR DELETE ON lexicon.v3_form_variants
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lexicon.v3_check_variant_form_trigger();
