-- Contracting V3 storage is safe only before any canonical V3 row or V3
-- publication exists. Once writers are enabled, rollback is code/config only;
-- stored V3 data must remain readable and must never be coerced into V2.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.entries WHERE content_schema_version = 3)
       OR EXISTS (SELECT 1 FROM lexicon.v3_entry_state)
       OR EXISTS (
           SELECT 1
           FROM lexicon.entry_publications
           WHERE content_schema_version = 3
       ) THEN
        RAISE EXCEPTION
            'cannot contract Smart Lexicon V3 storage while V3 rows exist';
    END IF;
END
$$;

DROP TRIGGER lexicon_v3_form_variants_regional_shape_trigger
    ON lexicon.v3_form_variants;
DROP TRIGGER lexicon_v3_concrete_forms_regional_shape_trigger
    ON lexicon.v3_concrete_forms;
DROP TRIGGER lexicon_v3_group_memberships_form_required_trigger
    ON lexicon.v3_group_memberships;
DROP TRIGGER lexicon_v3_concrete_forms_membership_required_trigger
    ON lexicon.v3_concrete_forms;

DROP FUNCTION lexicon.v3_check_variant_form_trigger();
DROP FUNCTION lexicon.v3_check_concrete_form_regional_trigger();
DROP FUNCTION lexicon.v3_assert_form_has_regional_shape(UUID);
DROP FUNCTION lexicon.v3_check_membership_form_trigger();
DROP FUNCTION lexicon.v3_check_concrete_form_membership_trigger();
DROP FUNCTION lexicon.v3_assert_form_has_membership(UUID);

DROP TABLE lexicon.v3_pronunciations;
DROP TABLE lexicon.v3_form_variants;
DROP TABLE lexicon.v3_group_memberships;
DROP TABLE lexicon.v3_concrete_forms;
DROP TABLE lexicon.v3_form_groups;
DROP TABLE lexicon.v3_entry_state;

ALTER TABLE lexicon.entry_publication_nodes
    DROP CONSTRAINT lexicon_entry_publication_nodes_type_check,
    ADD CONSTRAINT lexicon_entry_publication_nodes_type_check CHECK (node_type IN (
        'pos', 'form_group', 'form_slot', 'form_variant', 'pronunciation',
        'sense_group', 'grammar_structure', 'sense', 'definition', 'sentence',
        'text_variant', 'relation'
    ));

ALTER TABLE lexicon.entry_publications
    DROP CONSTRAINT lexicon_entry_publications_schema_version_check,
    DROP CONSTRAINT lexicon_entry_publications_entry_schema_revision_key,
    ADD CONSTRAINT lexicon_entry_publications_schema_version_check
        CHECK (content_schema_version = 2),
    ADD CONSTRAINT lexicon_entry_publications_entry_revision_key
        UNIQUE (entry_id, source_revision);

ALTER TABLE lexicon.nodes
    DROP CONSTRAINT lexicon_nodes_type_check,
    ADD CONSTRAINT lexicon_nodes_type_check CHECK (node_type IN (
        'pos', 'form_group', 'form_slot', 'form_variant', 'pronunciation',
        'sense_group', 'grammar_structure', 'sense', 'definition', 'sentence',
        'text_variant', 'relation'
    ));

DROP INDEX lexicon.lexicon_entry_pos_v3_ordinal_key;

ALTER TABLE lexicon.entry_pos
    DROP CONSTRAINT lexicon_entry_pos_entry_schema_fkey,
    DROP CONSTRAINT lexicon_entry_pos_id_entry_schema_key,
    DROP CONSTRAINT lexicon_entry_pos_versioned_modes_check,
    DROP CONSTRAINT lexicon_entry_pos_schema_version_check,
    ALTER COLUMN spelling_mode SET NOT NULL,
    ALTER COLUMN phonetic_mode SET NOT NULL,
    DROP COLUMN content_schema_version,
    ADD CONSTRAINT lexicon_entry_pos_modes_check CHECK (
        spelling_mode <> 'distinguish' OR phonetic_mode = 'distinguish'
    );

ALTER TABLE lexicon.entries
    DROP CONSTRAINT lexicon_entries_id_schema_version_key,
    DROP CONSTRAINT lexicon_entries_versioned_headword_shape_check,
    DROP CONSTRAINT lexicon_entries_schema_kind_check,
    DROP CONSTRAINT lexicon_entries_schema_version_check,
    ALTER COLUMN headword_mode SET NOT NULL,
    ADD CONSTRAINT lexicon_entries_schema_version_check
        CHECK (content_schema_version = 2),
    ADD CONSTRAINT lexicon_entries_headword_shape_check CHECK (
        (headword_mode = 'unified' AND source_dialect IS NULL)
        OR (headword_mode = 'distinguish' AND source_dialect IS NOT NULL)
    );
