UPDATE catalog.metadata
SET version = GREATEST(1, version - 1), updated_at = now()
WHERE id = TRUE;

DROP TABLE lexicon.consumed_detections;
