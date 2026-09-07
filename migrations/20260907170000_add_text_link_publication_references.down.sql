-- 有正文关联引用时由约束拒绝回退，避免丢失已发布的引用保护。
ALTER TABLE lexicon.entry_publication_sense_refs
    DROP CONSTRAINT lexicon_publication_sense_refs_kind_check,
    ADD CONSTRAINT lexicon_publication_sense_refs_kind_check CHECK (
        reference_kind IN ('relation', 'sentence_context', 'phrase_component')
    );
