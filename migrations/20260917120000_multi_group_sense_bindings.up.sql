-- Preserve existing single bindings and constrain every new edge to the same POS.
ALTER TABLE lexicon.senses ADD CONSTRAINT lexicon_senses_id_pos_entry_key
    UNIQUE (id, entry_pos_id, entry_id);
CREATE TABLE lexicon.sense_form_group_bindings (
    sense_id UUID NOT NULL,
    form_group_id UUID NOT NULL,
    entry_pos_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    PRIMARY KEY (sense_id, form_group_id),
    FOREIGN KEY (sense_id, entry_pos_id, entry_id)
        REFERENCES lexicon.senses(id, entry_pos_id, entry_id) ON DELETE CASCADE,
    FOREIGN KEY (form_group_id, entry_pos_id, entry_id)
        REFERENCES lexicon.v3_form_groups(id, entry_pos_id, entry_id)
        DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX ON lexicon.sense_form_group_bindings(entry_id);
INSERT INTO lexicon.sense_form_group_bindings(sense_id, form_group_id, entry_pos_id, entry_id)
SELECT id, form_group_id, entry_pos_id, entry_id FROM lexicon.senses
WHERE form_group_id IS NOT NULL;
