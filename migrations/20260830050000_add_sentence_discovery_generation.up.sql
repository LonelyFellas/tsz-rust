-- Transaction-serialized watermark for sentence target discovery projections.
-- Multiple surface projection statements in one transaction advance exactly once;
-- rollback also rolls the watermark back, so readers cannot observe commit-order gaps.

CREATE TABLE lexicon.sentence_discovery_generation (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    generation BIGINT NOT NULL DEFAULT 1 CHECK (generation > 0),
    last_txid BIGINT
);

INSERT INTO lexicon.sentence_discovery_generation (singleton, generation)
VALUES (TRUE, 1);

CREATE FUNCTION lexicon.bump_sentence_discovery_generation()
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

CREATE TRIGGER lexicon_surface_sources_discovery_generation_trigger
AFTER INSERT OR UPDATE OR DELETE ON lexicon.surface_sources
FOR EACH STATEMENT
EXECUTE FUNCTION lexicon.bump_sentence_discovery_generation();
