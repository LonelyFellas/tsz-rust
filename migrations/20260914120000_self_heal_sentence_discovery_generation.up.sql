-- Lexicon resets (`TRUNCATE lexicon.*`) also wipe the generation singleton, and
-- migrations never rerun. Upsert from the trigger so the next surface projection
-- write rebuilds it. A fresh watermark avoids restarting at 1 and reusing
-- pre-reset cursor versions; an existing row still advances once per transaction.
CREATE OR REPLACE FUNCTION lexicon.bump_sentence_discovery_generation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    current_txid BIGINT := txid_current();
BEGIN
    INSERT INTO lexicon.sentence_discovery_generation AS stored (singleton, generation, last_txid)
    VALUES (TRUE, (EXTRACT(EPOCH FROM clock_timestamp()) * 1000000)::BIGINT, current_txid)
    ON CONFLICT (singleton) DO UPDATE
    SET generation = CASE
            WHEN stored.last_txid IS DISTINCT FROM current_txid THEN stored.generation + 1
            ELSE stored.generation
        END,
        last_txid = current_txid;
    RETURN NULL;
END
$$;

-- Environments already missing the row recover on deploy; healthy ones are untouched.
INSERT INTO lexicon.sentence_discovery_generation (singleton, generation)
VALUES (TRUE, (EXTRACT(EPOCH FROM clock_timestamp()) * 1000000)::BIGINT)
ON CONFLICT (singleton) DO NOTHING;
