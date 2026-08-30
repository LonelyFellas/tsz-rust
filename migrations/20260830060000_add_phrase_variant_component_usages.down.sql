DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.v3_phrase_variant_component_usages)
       OR EXISTS (
           SELECT 1 FROM lexicon.nodes
           WHERE node_type = 'phrase_component_usage'
       )
       OR EXISTS (
           SELECT 1 FROM lexicon.entry_publication_nodes
           WHERE node_type = 'phrase_component_usage'
       )
       OR EXISTS (
           SELECT 1 FROM lexicon.entry_publication_sense_refs
           WHERE reference_kind = 'phrase_component'
       ) THEN
        RAISE EXCEPTION 'cannot remove phrase variant component usages while draft or publication data exists'
            USING ERRCODE = '0A000';
    END IF;
END
$$;

ALTER TABLE lexicon.entry_publication_sense_refs
    DROP CONSTRAINT lexicon_publication_sense_refs_kind_check,
    ADD CONSTRAINT lexicon_publication_sense_refs_kind_check CHECK (
        reference_kind IN ('relation', 'sentence_context')
    );

ALTER TABLE lexicon.entry_publication_nodes
    DROP CONSTRAINT lexicon_entry_publication_nodes_type_check,
    ADD CONSTRAINT lexicon_entry_publication_nodes_type_check CHECK (node_type IN (
        'pos', 'form_group', 'form_slot', 'concrete_form', 'group_membership',
        'form_variant', 'pronunciation', 'sense_group', 'grammar_structure',
        'sense', 'definition', 'sentence', 'text_variant', 'relation'
    ));

ALTER TABLE lexicon.nodes
    DROP CONSTRAINT lexicon_nodes_type_check,
    ADD CONSTRAINT lexicon_nodes_type_check CHECK (node_type IN (
        'pos', 'form_group', 'form_slot', 'concrete_form', 'group_membership',
        'form_variant', 'pronunciation', 'sense_group', 'grammar_structure',
        'sense', 'definition', 'sentence', 'text_variant', 'relation'
    ));

DROP TABLE lexicon.v3_phrase_variant_component_usages;
