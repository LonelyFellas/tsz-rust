-- 成分用词归属于短语 publication 的具体方言词形。common/uk/us 各自保存，
-- 即使拼写相同也不得复用另一侧配置。
CREATE TABLE lexicon.v3_phrase_variant_component_usages (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    form_variant_id UUID NOT NULL,
    ordinal SMALLINT NOT NULL
        CONSTRAINT lexicon_v3_phrase_components_ordinal_check
        CHECK (ordinal BETWEEN 0 AND 99),
    state TEXT NOT NULL
        CONSTRAINT lexicon_v3_phrase_components_state_check
        CHECK (state IN ('unresolved', 'resolved')),
    literal TEXT NOT NULL
        CONSTRAINT lexicon_v3_phrase_components_literal_check
        CHECK (
            literal = btrim(literal)
            AND char_length(literal) BETWEEN 1 AND 200
        ),
    target_entry_id UUID,
    target_publication_id UUID,
    target_pos_id UUID,
    target_base_form_id UUID,
    target_sense_id UUID,
    target_form_id UUID,
    target_variant_id UUID,
    target_dialect TEXT
        CONSTRAINT lexicon_v3_phrase_components_target_dialect_check
        CHECK (target_dialect IN ('common', 'uk', 'us')),
    target_form_type TEXT
        CONSTRAINT lexicon_v3_phrase_components_target_form_type_check
        CHECK (target_form_type IN (
            'base', 'present_participle', 'past_tense', 'past_participle',
            'third_person_singular', 'plural', 'comparative', 'superlative'
        )),
    target_headword_snapshot TEXT,
    target_gloss_snapshot TEXT,
    CONSTRAINT lexicon_v3_phrase_components_node_fkey
        FOREIGN KEY (id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_owner_fkey
        FOREIGN KEY (form_variant_id, entry_id)
        REFERENCES lexicon.v3_form_variants(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_phrase_components_publication_fkey
        FOREIGN KEY (target_publication_id, target_entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_pos_fkey
        FOREIGN KEY (target_pos_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_base_fkey
        FOREIGN KEY (target_base_form_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_sense_fkey
        FOREIGN KEY (target_sense_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_form_fkey
        FOREIGN KEY (target_form_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_variant_fkey
        FOREIGN KEY (target_variant_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_publication_pos_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_pos_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT lexicon_v3_phrase_components_publication_base_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_base_form_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT lexicon_v3_phrase_components_publication_sense_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_sense_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT lexicon_v3_phrase_components_publication_form_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_form_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT lexicon_v3_phrase_components_publication_variant_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_variant_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT lexicon_v3_phrase_components_variant_ordinal_key
        UNIQUE (form_variant_id, ordinal),
    CONSTRAINT lexicon_v3_phrase_components_variant_id_key
        UNIQUE (form_variant_id, id),
    CONSTRAINT lexicon_v3_phrase_components_shape_check CHECK (
        (
            state = 'unresolved'
            AND target_entry_id IS NULL
            AND target_publication_id IS NULL
            AND target_pos_id IS NULL
            AND target_base_form_id IS NULL
            AND target_sense_id IS NULL
            AND target_form_id IS NULL
            AND target_variant_id IS NULL
            AND target_dialect IS NULL
            AND target_form_type IS NULL
            AND target_headword_snapshot IS NULL
            AND target_gloss_snapshot IS NULL
        ) OR (
            state = 'resolved'
            AND target_entry_id IS NOT NULL
            AND target_publication_id IS NOT NULL
            AND target_pos_id IS NOT NULL
            AND target_base_form_id IS NOT NULL
            AND target_sense_id IS NOT NULL
            AND target_form_id IS NOT NULL
            AND target_variant_id IS NOT NULL
            AND target_dialect IS NOT NULL
            AND target_form_type IS NOT NULL
            AND target_headword_snapshot IS NOT NULL
            AND target_gloss_snapshot IS NOT NULL
        )
    )
);

CREATE INDEX lexicon_v3_phrase_components_entry_idx
    ON lexicon.v3_phrase_variant_component_usages (
        entry_id, form_variant_id, ordinal, id
    );

CREATE INDEX lexicon_v3_phrase_components_target_idx
    ON lexicon.v3_phrase_variant_component_usages (
        target_entry_id, target_publication_id, target_sense_id
    )
    WHERE state = 'resolved';

ALTER TABLE lexicon.entry_publication_nodes
    DROP CONSTRAINT lexicon_entry_publication_nodes_type_check,
    ADD CONSTRAINT lexicon_entry_publication_nodes_type_check CHECK (node_type IN (
        'pos', 'form_group', 'form_slot', 'concrete_form', 'group_membership',
        'form_variant', 'pronunciation', 'phrase_component_usage', 'sense_group',
        'grammar_structure', 'sense', 'definition', 'sentence', 'text_variant',
        'relation'
    ));

ALTER TABLE lexicon.nodes
    DROP CONSTRAINT lexicon_nodes_type_check,
    ADD CONSTRAINT lexicon_nodes_type_check CHECK (node_type IN (
        'pos', 'form_group', 'form_slot', 'concrete_form', 'group_membership',
        'form_variant', 'pronunciation', 'phrase_component_usage', 'sense_group',
        'grammar_structure', 'sense', 'definition', 'sentence', 'text_variant',
        'relation'
    ));

ALTER TABLE lexicon.entry_publication_sense_refs
    DROP CONSTRAINT lexicon_publication_sense_refs_kind_check,
    ADD CONSTRAINT lexicon_publication_sense_refs_kind_check CHECK (
        reference_kind IN ('relation', 'sentence_context', 'phrase_component')
    );
