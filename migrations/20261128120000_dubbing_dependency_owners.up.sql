-- Route each polymorphic owner to its actual table, including on direct SQL deletes.
ALTER TABLE speech.dubbing_dependencies
    ADD COLUMN entry_owner_id UUID GENERATED ALWAYS AS
        (CASE WHEN owner_kind = 'entry' THEN owner_id END) STORED,
    ADD COLUMN sentence_owner_id UUID GENERATED ALWAYS AS
        (CASE WHEN owner_kind = 'sentence' THEN owner_id END) STORED;

CREATE INDEX dubbing_dependencies_entry_owner
    ON speech.dubbing_dependencies(entry_owner_id) WHERE entry_owner_id IS NOT NULL;
CREATE INDEX dubbing_dependencies_sentence_owner
    ON speech.dubbing_dependencies(sentence_owner_id) WHERE sentence_owner_id IS NOT NULL;

-- Enforce ownership for new registrations without deleting any legacy orphan rows.
ALTER TABLE speech.dubbing_dependencies
    ADD CONSTRAINT dubbing_dependencies_entry_owner_fk FOREIGN KEY (entry_owner_id)
        REFERENCES lexicon.entries(id) ON DELETE CASCADE NOT VALID,
    ADD CONSTRAINT dubbing_dependencies_sentence_owner_fk FOREIGN KEY (sentence_owner_id)
        REFERENCES lexicon.shared_sentences(id) ON DELETE CASCADE NOT VALID;
