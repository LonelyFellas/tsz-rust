ALTER TABLE lexicon.sentence_associations
    ADD COLUMN target_publication_id UUID,
    ADD COLUMN target_form_variant_id UUID,
    ADD COLUMN target_component_usages_snapshot JSONB,
    ADD CONSTRAINT lexicon_sentence_associations_variant_identity_shape_check
        CHECK (
            (target_publication_id IS NULL) = (target_form_variant_id IS NULL)
            AND (target_publication_id IS NULL) = (target_component_usages_snapshot IS NULL)
            AND (state = 'linked' OR target_publication_id IS NULL)
            AND (
                target_component_usages_snapshot IS NULL
                OR jsonb_typeof(target_component_usages_snapshot) = 'array'
            )
        ),
    ADD CONSTRAINT lexicon_sentence_associations_target_publication_fkey
        FOREIGN KEY (target_publication_id, target_entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE RESTRICT,
    ADD CONSTRAINT lexicon_sentence_associations_target_variant_fkey
        FOREIGN KEY (target_form_variant_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    ADD CONSTRAINT lexicon_sentence_associations_target_publication_variant_fkey
        FOREIGN KEY (
            target_publication_id, target_entry_id, target_form_variant_id
        ) REFERENCES lexicon.entry_publication_nodes(
            publication_id, entry_id, node_id
        ) ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT lexicon_sentence_associations_target_publication_sense_fkey
        FOREIGN KEY (
            target_publication_id, target_entry_id, target_sense_id
        ) REFERENCES lexicon.entry_publication_nodes(
            publication_id, entry_id, node_id
        ) ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT lexicon_sentence_associations_target_publication_form_fkey
        FOREIGN KEY (
            target_publication_id, target_entry_id, target_form_slot_id
        ) REFERENCES lexicon.entry_publication_nodes(
            publication_id, entry_id, node_id
        ) ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE;

CREATE INDEX lexicon_sentence_associations_target_variant_idx
    ON lexicon.sentence_associations (
        target_entry_id, target_publication_id, target_form_variant_id
    )
    WHERE target_form_variant_id IS NOT NULL;
