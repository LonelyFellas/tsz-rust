-- A shared sentence remains global; sense collections are independent references.
-- Existing entry collections remain unassigned instead of guessing a sense.
CREATE TABLE lexicon.shared_sentence_sense_collections (
    sentence_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    sense_id UUID NOT NULL,
    PRIMARY KEY (sentence_id, entry_id, sense_id),
    FOREIGN KEY (sentence_id, entry_id)
        REFERENCES lexicon.shared_sentence_collections(sentence_id, entry_id) ON DELETE CASCADE,
    FOREIGN KEY (sense_id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE
);
CREATE INDEX shared_sentence_sense_collection_idx
    ON lexicon.shared_sentence_sense_collections(entry_id, sense_id);
