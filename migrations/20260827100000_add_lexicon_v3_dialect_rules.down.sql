-- Restore the pre-V3-dialect-rules active-draft shape. Immutable publication
-- snapshots were not changed by the up migration and need no rollback.

ALTER TABLE lexicon.entry_pos
    DROP CONSTRAINT lexicon_entry_pos_versioned_modes_check;

WITH rebuilt AS (
    SELECT projection.entry_id,
           jsonb_set(
               projection.forms,
               '{pos}',
               COALESCE((
                   SELECT jsonb_agg((pos_item - 'dialect_rules') ORDER BY pos_ordinal)
                   FROM jsonb_array_elements(
                       COALESCE(projection.forms -> 'pos', '[]'::jsonb)
                   ) WITH ORDINALITY AS pos_rows(pos_item, pos_ordinal)
               ), '[]'::jsonb),
               false
           ) AS forms
    FROM lexicon.entry_editor_projection AS projection
    JOIN lexicon.entries AS entry ON entry.id = projection.entry_id
    WHERE entry.content_schema_version = 3
)
UPDATE lexicon.entry_editor_projection AS projection
SET forms = rebuilt.forms
FROM rebuilt
WHERE projection.entry_id = rebuilt.entry_id;

UPDATE lexicon.entry_pos
SET spelling_mode = NULL,
    phonetic_mode = NULL
WHERE content_schema_version = 3;

ALTER TABLE lexicon.entry_pos
    ADD CONSTRAINT lexicon_entry_pos_versioned_modes_check CHECK (
        content_schema_version NOT IN (2, 3)
        OR (
            content_schema_version = 2
            AND spelling_mode IS NOT NULL
            AND phonetic_mode IS NOT NULL
            AND (spelling_mode <> 'distinguish' OR phonetic_mode = 'distinguish')
        )
        OR (
            content_schema_version = 3
            AND spelling_mode IS NULL
            AND phonetic_mode IS NULL
        )
    );
