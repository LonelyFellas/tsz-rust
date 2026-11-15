CREATE TABLE lexicon_next.sentence_taxonomy_refs (
    sentence_id uuid NOT NULL REFERENCES lexicon_next.sentences(id) ON DELETE CASCADE,
    taxonomy_id uuid NOT NULL REFERENCES catalog.lexicon_taxonomies(id) ON DELETE RESTRICT,
    PRIMARY KEY (sentence_id, taxonomy_id)
);
CREATE INDEX ON lexicon_next.sentence_taxonomy_refs(taxonomy_id);
