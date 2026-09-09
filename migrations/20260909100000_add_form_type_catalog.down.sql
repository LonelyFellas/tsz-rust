-- A historical publication can be the only remaining custom reference.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.entry_publication_form_type_refs WHERE form_type NOT IN (
        'base', 'third_person_singular', 'present_participle', 'past_tense',
        'past_participle', 'plural', 'comparative', 'superlative'
    )) THEN RAISE EXCEPTION 'custom form types are still referenced by historical publications'; END IF;
END $$;
-- Refuse rollback while custom form types remain in stored content.
ALTER TABLE lexicon.v3_concrete_forms DROP CONSTRAINT lexicon_v3_concrete_forms_type_check, ADD CONSTRAINT lexicon_v3_concrete_forms_type_check CHECK (form_type IN (
            'base', 'present_participle', 'past_tense', 'past_participle',
            'third_person_singular', 'plural', 'comparative', 'superlative'
        ));
ALTER TABLE lexicon.v3_concrete_forms DROP CONSTRAINT v3_concrete_forms_form_type_catalog_fkey;
DROP INDEX lexicon.v3_concrete_forms_form_type_idx;
ALTER TABLE lexicon.form_slots DROP CONSTRAINT lexicon_form_slots_type_check, ADD CONSTRAINT lexicon_form_slots_type_check CHECK (form_type IN (
            'base', 'present_participle', 'past_tense', 'past_participle',
            'third_person_singular', 'plural', 'comparative', 'superlative'
        ));
ALTER TABLE lexicon.form_slots DROP CONSTRAINT form_slots_form_type_catalog_fkey;
DROP INDEX lexicon.form_slots_form_type_idx;
ALTER TABLE lexicon.surface_sources DROP CONSTRAINT lexicon_surface_sources_source_shape_check, ADD CONSTRAINT lexicon_surface_sources_source_shape_check CHECK (
        (
            content_schema_version = 2
            AND source_kind IN ('headword', 'form')
            AND form_id IS NULL
            AND variant_id IS NULL
            AND group_ids IS NULL
            AND projection_version IS NULL
            AND (
                (
                    source_kind = 'headword'
                    AND source_node_id IS NULL
                    AND pos_id IS NULL
                    AND pos IS NULL
                    AND form_type IS NULL
                )
                OR
                (
                    source_kind = 'form'
                    AND source_node_id IS NOT NULL
                    AND pos_id IS NOT NULL
                    AND pos IS NOT NULL
                    AND btrim(pos) <> ''
                    AND form_type IS NOT NULL
                    AND form_type IN (
                        'base', 'present_participle', 'past_tense', 'past_participle',
                        'third_person_singular', 'plural', 'comparative', 'superlative'
                    )
                )
            )
        )
        OR
        (
            content_schema_version = 3
            AND source_kind = 'form_variant'
            AND source_node_id IS NOT NULL
            AND source_node_id = variant_id
            AND pos_id IS NOT NULL
            AND pos IS NOT NULL
            AND btrim(pos) <> ''
            AND form_id IS NOT NULL
            AND variant_id IS NOT NULL
            AND group_ids IS NOT NULL
            AND cardinality(group_ids) > 0
            AND array_position(group_ids, NULL) IS NULL
            AND projection_version IS NOT NULL
            AND projection_version = btrim(projection_version)
            AND char_length(projection_version) BETWEEN 1 AND 100
            AND form_type IS NOT NULL
            AND form_type IN (
                'base', 'present_participle', 'past_tense', 'past_participle',
                'third_person_singular', 'plural', 'comparative', 'superlative'
            )
            AND (
                (dialect = 'common' AND dialect_scope IN ('uk', 'us'))
                OR (dialect = 'uk' AND dialect_scope = 'uk')
                OR (dialect = 'us' AND dialect_scope = 'us')
            )
        )
    );
ALTER TABLE lexicon.sentence_associations DROP CONSTRAINT lexicon_sentence_associations_form_type_check, ADD CONSTRAINT lexicon_sentence_associations_form_type_check CHECK (
            resolved_form_type IN (
                'base', 'present_participle', 'past_tense', 'past_participle',
                'third_person_singular', 'plural', 'comparative', 'superlative'
            )
        );
ALTER TABLE lexicon.v3_phrase_variant_component_usages DROP CONSTRAINT lexicon_v3_phrase_components_target_form_type_check, ADD CONSTRAINT lexicon_v3_phrase_components_target_form_type_check CHECK (target_form_type IN (
            'base', 'present_participle', 'past_tense', 'past_participle',
            'third_person_singular', 'plural', 'comparative', 'superlative'
        ));
ALTER TABLE lexicon.v3_phrase_sense_component_usages DROP CONSTRAINT lexicon_v3_phrase_components_target_form_type_check, ADD CONSTRAINT lexicon_v3_phrase_components_target_form_type_check CHECK (target_form_type IN (
            'base', 'present_participle', 'past_tense', 'past_participle',
            'third_person_singular', 'plural', 'comparative', 'superlative'
        ));
DROP TABLE lexicon.entry_publication_form_type_refs;
DROP TABLE catalog.form_types;
DROP FUNCTION catalog.protect_form_type_identity();
UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
