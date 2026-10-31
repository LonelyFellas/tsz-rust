DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM speech.dubbing_group_pairings) THEN
        RAISE EXCEPTION 'Cannot discard saved dubbing pair identities; retain this schema';
    END IF;
END $$;
DROP TABLE speech.dubbing_group_pairings;
ALTER TABLE speech.dubbing_groups DROP COLUMN order_revision;
