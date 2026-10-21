-- Keep explicit cross-region pronunciation identity after slots are retired.
CREATE TABLE lexicon.v3_form_pronunciation_mappings (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    content_id UUID NOT NULL,
    UNIQUE (id, entry_id, content_id),
    FOREIGN KEY (content_id, entry_id, content_id)
        REFERENCES lexicon.v3_form_content_slots(id, entry_id, content_id)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE lexicon.v3_form_pronunciation_mapping_slots (
    slot_id UUID PRIMARY KEY,
    mapping_id UUID NOT NULL,
    entry_id UUID NOT NULL REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    content_id UUID NOT NULL,
    region TEXT NOT NULL CHECK (region IN ('common', 'uk', 'us')),
    UNIQUE (mapping_id, region),
    FOREIGN KEY (mapping_id, entry_id, content_id)
        REFERENCES lexicon.v3_form_pronunciation_mappings(id, entry_id, content_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (slot_id, entry_id, content_id)
        REFERENCES lexicon.v3_form_content_slots(id, entry_id, content_id)
        DEFERRABLE INITIALLY DEFERRED
);
