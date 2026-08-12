DROP INDEX lexicon.lexicon_nodes_parent_idx;
DROP INDEX lexicon.lexicon_nodes_stable_slot_key;

ALTER TABLE lexicon.nodes
    DROP CONSTRAINT lexicon_nodes_parent_fkey,
    DROP CONSTRAINT lexicon_nodes_stable_slot_parent_check,
    DROP CONSTRAINT lexicon_nodes_role_nonempty_check,
    DROP COLUMN stable_slot,
    DROP COLUMN node_role,
    DROP COLUMN parent_node_id;
