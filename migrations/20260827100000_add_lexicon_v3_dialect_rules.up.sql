-- Persist the V3 POS-level spelling/phonetic intent without rewriting immutable
-- publication snapshots. Active editor JSON is backfilled in array order; the
-- existing stable POS/form/variant/group/membership UUIDs are untouched.

ALTER TABLE lexicon.entry_pos
    DROP CONSTRAINT lexicon_entry_pos_versioned_modes_check;

WITH rebuilt AS (
    SELECT projection.entry_id,
           jsonb_set(
               projection.forms,
               '{pos}',
               COALESCE((
                   SELECT jsonb_agg(
                       CASE
                           WHEN pos_item ? 'dialect_rules' THEN pos_item
                           ELSE jsonb_set(
                               pos_item,
                               '{dialect_rules}',
                               CASE
                                   WHEN COALESCE(
                                       pos_item #>> '{forms,0,regional_variants,mode}',
                                       'common'
                                   ) = 'common'
                                   THEN jsonb_build_object(
                                       'spelling_mode', 'unified',
                                       'phonetic_mode', 'unified'
                                   )
                                   WHEN NOT EXISTS (
                                       SELECT 1
                                       FROM jsonb_array_elements(
                                           COALESCE(pos_item -> 'forms', '[]'::jsonb)
                                       ) AS form_item
                                       WHERE form_item #>> '{regional_variants,mode}' = 'uk_us'
                                         AND form_item #>> '{regional_variants,uk,spelling}'
                                             IS DISTINCT FROM
                                             form_item #>> '{regional_variants,us,spelling}'
                                   )
                                   THEN jsonb_build_object(
                                       'spelling_mode', 'unified',
                                       'phonetic_mode', 'distinguish'
                                   )
                                   ELSE jsonb_build_object(
                                       'spelling_mode', 'distinguish',
                                       'phonetic_mode', 'distinguish'
                                   )
                               END,
                               true
                           )
                       END
                       ORDER BY pos_ordinal
                   )
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

UPDATE lexicon.entry_pos AS entry_pos
SET spelling_mode = pos_item #>> '{dialect_rules,spelling_mode}',
    phonetic_mode = pos_item #>> '{dialect_rules,phonetic_mode}'
FROM lexicon.entry_editor_projection AS projection
CROSS JOIN LATERAL jsonb_array_elements(
    COALESCE(projection.forms -> 'pos', '[]'::jsonb)
) AS pos_rows(pos_item)
WHERE entry_pos.entry_id = projection.entry_id
  AND entry_pos.content_schema_version = 3
  AND entry_pos.id::text = pos_item ->> 'pos_id';

ALTER TABLE lexicon.entry_pos
    ADD CONSTRAINT lexicon_entry_pos_versioned_modes_check CHECK (
        content_schema_version NOT IN (2, 3)
        OR (
            content_schema_version IN (2, 3)
            AND spelling_mode IS NOT NULL
            AND phonetic_mode IS NOT NULL
            AND (spelling_mode <> 'distinguish' OR phonetic_mode = 'distinguish')
        )
    );
