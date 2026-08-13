CREATE TABLE lexicon.consumed_detections (
    actor_id     UUID NOT NULL REFERENCES admins(id) ON DELETE RESTRICT,
    detection_id UUID NOT NULL,
    entry_id     UUID UNIQUE,
    consumed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (actor_id, detection_id),
    FOREIGN KEY (entry_id) REFERENCES lexicon.entries(id) ON DELETE SET NULL
        DEFERRABLE INITIALLY DEFERRED
);
