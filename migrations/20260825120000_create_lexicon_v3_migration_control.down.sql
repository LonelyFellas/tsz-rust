-- Never discard migration evidence while a migrated current aggregate is live.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon.v3_migration_batches
        WHERE status <> 'rolled_back'
    ) OR EXISTS (
        SELECT 1
        FROM lexicon.v3_migration_entries
        WHERE status IN ('planned', 'applied', 'verified')
    ) OR EXISTS (
        SELECT 1
        FROM lexicon.v3_entry_state
        WHERE origin = 'migrated_v2'
    ) OR EXISTS (
        SELECT 1
        FROM lexicon.entry_publications
        WHERE content_schema_version = 3
    ) THEN
        RAISE EXCEPTION
            'cannot remove Smart Lexicon V3 migration control while live migrations exist';
    END IF;
END
$$;

DROP TABLE lexicon.v3_migration_map;
DROP TABLE lexicon.v3_migration_entries;
DROP TABLE lexicon.v3_migration_batches;
