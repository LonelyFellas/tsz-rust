CREATE TEMP TABLE retired_prebinding_sources ON COMMIT DROP AS
SELECT entry_id FROM lexicon.relations WHERE prebound_target_entry_id IS NOT NULL
UNION
SELECT entry_id FROM lexicon.entry_editor_projection
WHERE jsonb_path_exists(meanings, '$.pos[*].senses[*].relations[*] ? (@.prebound_target_word_id != null)');

-- Prebinding is retired. Keep explicit sense bindings and unlinked display text.
UPDATE lexicon.nodes node
SET removed_from_draft_at = COALESCE(node.removed_from_draft_at, now())
FROM lexicon.relations relation
WHERE node.id = relation.id
  AND node.entry_id = relation.entry_id
  AND relation.prebound_target_entry_id IS NOT NULL;

DELETE FROM lexicon.relations WHERE prebound_target_entry_id IS NOT NULL;

UPDATE lexicon.entry_editor_projection projection
SET meanings = jsonb_set(projection.meanings, '{pos}', (
    SELECT COALESCE(jsonb_agg(
        jsonb_set(pos_item.value, '{senses}', (
            SELECT COALESCE(jsonb_agg(
                jsonb_set(sense_item.value, '{relations}', (
                    SELECT COALESCE(jsonb_agg(relation_item.value ORDER BY relation_item.ordinality), '[]'::jsonb)
                    FROM jsonb_array_elements(sense_item.value -> 'relations')
                         WITH ORDINALITY AS relation_item
                    WHERE relation_item.value ->> 'prebound_target_word_id' IS NULL
                )) ORDER BY sense_item.ordinality
            ), '[]'::jsonb)
            FROM jsonb_array_elements(pos_item.value -> 'senses') WITH ORDINALITY AS sense_item
        )) ORDER BY pos_item.ordinality
    ), '[]'::jsonb)
    FROM jsonb_array_elements(projection.meanings -> 'pos') WITH ORDINALITY AS pos_item
))
WHERE jsonb_path_exists(projection.meanings, '$.pos[*].senses[*].relations[*] ? (@.prebound_target_word_id != null)');

-- Retain old nullable columns for historical schema compatibility; no new row may use them.
ALTER TABLE lexicon.relations
    ADD CONSTRAINT lexicon_relations_no_prebinding_check
    CHECK (prebound_target_entry_id IS NULL AND prebinding_reason IS NULL);

-- Invalidate stale editors and keep unchanged presentation/surface projections current.
UPDATE lexicon.entries SET revision = revision + 1, updated_at = now()
WHERE id IN (SELECT entry_id FROM retired_prebinding_sources);
UPDATE lexicon.entry_editor_projection p
SET rebuilt_revision = e.revision, updated_at = now()
FROM lexicon.entries e
WHERE p.entry_id = e.id AND e.id IN (SELECT entry_id FROM retired_prebinding_sources);
UPDATE lexicon.entry_presentation_projection p
SET source_revision = e.revision, updated_at = now()
FROM lexicon.entries e
WHERE p.entry_id = e.id AND e.id IN (SELECT entry_id FROM retired_prebinding_sources);
UPDATE lexicon.surface_sources p
SET source_revision = e.revision, updated_at = now()
FROM lexicon.entries e
WHERE p.entry_id = e.id AND p.content_scope = 'draft'
  AND e.id IN (SELECT entry_id FROM retired_prebinding_sources);
