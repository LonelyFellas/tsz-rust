ALTER TABLE lexicon.nodes
    ADD COLUMN parent_node_id UUID,
    ADD COLUMN node_role TEXT NOT NULL DEFAULT 'legacy',
    ADD COLUMN stable_slot BOOLEAN NOT NULL DEFAULT FALSE;

UPDATE lexicon.nodes AS node
SET parent_node_id = NULL,
    node_role = 'forms.pos'
FROM lexicon.entry_pos AS pos
WHERE pos.id = node.id AND pos.entry_id = node.entry_id;

UPDATE lexicon.nodes AS node
SET parent_node_id = form_group.entry_pos_id,
    node_role = 'forms.form_group'
FROM lexicon.form_groups AS form_group
WHERE form_group.id = node.id AND form_group.entry_id = node.entry_id;

UPDATE lexicon.nodes AS node
SET parent_node_id = slot.entry_pos_id,
    node_role = 'forms.base_form',
    stable_slot = TRUE
FROM lexicon.form_slots AS slot
WHERE slot.id = node.id
  AND slot.entry_id = node.entry_id
  AND slot.form_group_id IS NULL;

UPDATE lexicon.nodes AS node
SET parent_node_id = slot.form_group_id,
    node_role = 'forms.form_slot:' || slot.form_type,
    stable_slot = TRUE
FROM lexicon.form_slots AS slot
WHERE slot.id = node.id
  AND slot.entry_id = node.entry_id
  AND slot.form_group_id IS NOT NULL;

UPDATE lexicon.nodes AS node
SET parent_node_id = variant.form_slot_id,
    node_role = 'forms.form_variant:' || variant.dialect,
    stable_slot = TRUE
FROM lexicon.form_variants AS variant
WHERE variant.id = node.id AND variant.entry_id = node.entry_id;

UPDATE lexicon.nodes AS node
SET parent_node_id = pronunciation.form_variant_id,
    node_role = 'forms.pronunciation'
FROM lexicon.pronunciations AS pronunciation
WHERE pronunciation.id = node.id AND pronunciation.entry_id = node.entry_id;

UPDATE lexicon.nodes AS node
SET parent_node_id = NULL,
    node_role = 'meanings.sense_group'
FROM lexicon.sense_groups AS sense_group
WHERE sense_group.id = node.id AND sense_group.entry_id = node.entry_id;

UPDATE lexicon.nodes AS node
SET parent_node_id = grammar.entry_pos_id,
    node_role = 'meanings.grammar_structure'
FROM lexicon.grammar_structures AS grammar
WHERE grammar.id = node.id AND grammar.entry_id = node.entry_id;

UPDATE lexicon.nodes AS node
SET parent_node_id = sense.entry_pos_id,
    node_role = 'meanings.sense'
FROM lexicon.senses AS sense
WHERE sense.id = node.id AND sense.entry_id = node.entry_id;

UPDATE lexicon.nodes AS node
SET parent_node_id = definition.sense_id,
    node_role = 'meanings.definition:' || definition.language || ':' || definition.definition_kind
FROM lexicon.definitions AS definition
WHERE definition.id = node.id AND definition.entry_id = node.entry_id;

UPDATE lexicon.nodes AS node
SET parent_node_id = sentence.sense_id,
    node_role = 'meanings.sentence'
FROM lexicon.sentences AS sentence
WHERE sentence.id = node.id AND sentence.entry_id = node.entry_id;

UPDATE lexicon.nodes AS node
SET parent_node_id = variant.owner_node_id,
    node_role = 'meanings.' || variant.field_role || ':' || variant.language || ':' || variant.dialect,
    stable_slot = TRUE
FROM lexicon.text_variants AS variant
WHERE variant.id = node.id AND variant.entry_id = node.entry_id;

UPDATE lexicon.nodes AS node
SET parent_node_id = relation.source_sense_id,
    node_role = 'meanings.relation'
FROM lexicon.relations AS relation
WHERE relation.id = node.id AND relation.entry_id = node.entry_id;

ALTER TABLE lexicon.nodes
    ADD CONSTRAINT lexicon_nodes_role_nonempty_check
        CHECK (node_role <> ''),
    ADD CONSTRAINT lexicon_nodes_stable_slot_parent_check
        CHECK (NOT stable_slot OR parent_node_id IS NOT NULL),
    ADD CONSTRAINT lexicon_nodes_parent_fkey
        FOREIGN KEY (parent_node_id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id)
        ON DELETE CASCADE;

CREATE UNIQUE INDEX lexicon_nodes_stable_slot_key
    ON lexicon.nodes (entry_id, parent_node_id, node_role)
    WHERE stable_slot;

CREATE INDEX lexicon_nodes_parent_idx
    ON lexicon.nodes (entry_id, parent_node_id, node_role);
