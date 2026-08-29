-- Smart Lexicon V3 was not released before dialect_rules became mandatory.
-- The approved rollout resets Smart Lexicon business data and supports only
-- the latest contract; fail closed instead of carrying legacy V3 shapes.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon.entries
        WHERE content_schema_version = 3
    ) THEN
        RAISE EXCEPTION 'legacy V3 data is unsupported; reset Smart Lexicon before applying the latest dialect contract';
    END IF;
END
$$;
