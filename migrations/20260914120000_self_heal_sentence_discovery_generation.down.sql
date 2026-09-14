-- Restore the update-only trigger. Keep the singleton row: removing it breaks
-- discovery in both the current and previous application versions.
CREATE OR REPLACE FUNCTION lexicon.bump_sentence_discovery_generation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    current_txid BIGINT := txid_current();
BEGIN
    UPDATE lexicon.sentence_discovery_generation
    SET generation = CASE
            WHEN last_txid IS DISTINCT FROM current_txid THEN generation + 1
            ELSE generation
        END,
        last_txid = current_txid
    WHERE singleton = TRUE;
    RETURN NULL;
END
$$;
