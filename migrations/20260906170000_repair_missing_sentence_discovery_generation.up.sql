-- Repair environments where the singleton was lost after its original migration.
-- A fresh watermark avoids restarting at 1 and reusing pre-repair cursor versions.
-- Healthy environments retain their watermark and per-transaction bump state.
INSERT INTO lexicon.sentence_discovery_generation (singleton, generation)
VALUES (TRUE, (EXTRACT(EPOCH FROM clock_timestamp()) * 1000000)::BIGINT)
ON CONFLICT (singleton) DO NOTHING;
