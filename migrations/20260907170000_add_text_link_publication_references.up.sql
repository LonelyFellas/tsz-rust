ALTER TABLE lexicon.entry_publication_sense_refs
    DROP CONSTRAINT lexicon_publication_sense_refs_kind_check,
    ADD CONSTRAINT lexicon_publication_sense_refs_kind_check CHECK (
        reference_kind IN ('relation', 'sentence_context', 'phrase_component', 'text_link')
    );
