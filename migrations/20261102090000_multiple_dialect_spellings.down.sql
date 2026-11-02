-- Snapshots remain authoritative even when each side has only one surviving spelling.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM lexicon.v3_form_contents
        WHERE content #>> '{regional_variants,mode}' = 'uk_us_many'
    ) OR EXISTS (
        SELECT 1 FROM lexicon.entry_editor_projection
        WHERE jsonb_path_exists(forms, '$.contents[*].regional_variants ? (@.mode == "uk_us_many")')
           OR jsonb_path_exists(forms, '$.pos[*].forms[*].regional_variants ? (@.mode == "uk_us_many")')
    ) OR EXISTS (
        SELECT 1 FROM lexicon.entry_publications
        WHERE jsonb_path_exists(snapshot, '$.forms.contents[*].regional_variants ? (@.mode == "uk_us_many")')
           OR jsonb_path_exists(snapshot, '$.forms.pos[*].forms[*].regional_variants ? (@.mode == "uk_us_many")')
    ) THEN
        RAISE EXCEPTION 'multiple spelling snapshots must be migrated before downgrading'
            USING ERRCODE = '23514';
    END IF;
END
$$;

-- Fail transactionally if multiple (including retired) identities would be lost.
ALTER TABLE lexicon.v3_form_variants
    ADD CONSTRAINT lexicon_v3_form_variants_form_dialect_key UNIQUE (form_id, dialect);
CREATE UNIQUE INDEX lexicon_v3_form_content_slots_region_key
    ON lexicon.v3_form_content_slots(content_id, role)
    WHERE role IN ('common', 'uk', 'us');
DROP INDEX lexicon.lexicon_nodes_stable_slot_key;
CREATE UNIQUE INDEX lexicon_nodes_stable_slot_key
    ON lexicon.nodes (entry_id, parent_node_id, node_role)
    WHERE stable_slot;

CREATE OR REPLACE FUNCTION lexicon.v3_assert_form_has_regional_shape(target_form_id UUID)
RETURNS VOID
LANGUAGE plpgsql
AS $$
DECLARE
    variant_count INTEGER;
    common_count INTEGER;
    uk_count INTEGER;
    us_count INTEGER;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM lexicon.v3_concrete_forms WHERE id = target_form_id
    ) THEN
        RETURN;
    END IF;

    SELECT
        count(*)::INTEGER,
        count(*) FILTER (WHERE dialect = 'common')::INTEGER,
        count(*) FILTER (WHERE dialect = 'uk')::INTEGER,
        count(*) FILTER (WHERE dialect = 'us')::INTEGER
    INTO variant_count, common_count, uk_count, us_count
    FROM lexicon.v3_form_variants
    WHERE form_id = target_form_id;

    IF NOT (
        (variant_count = 1 AND common_count = 1)
        OR (variant_count = 2 AND uk_count = 1 AND us_count = 1)
    ) THEN
        RAISE EXCEPTION 'V3 concrete form % must have common xor complete uk/us variants', target_form_id
            USING ERRCODE = '23514',
                  CONSTRAINT = 'lexicon_v3_form_variants_regional_shape_check';
    END IF;
END
$$;
