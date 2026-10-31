-- Keep the original side settings and version IDs as the single parameter source.
-- No legacy pair can be inferred from names or positions.
ALTER TABLE speech.dubbing_groups
    ADD COLUMN order_revision BIGINT NOT NULL DEFAULT 0 CHECK (order_revision >= 0);

CREATE TABLE speech.dubbing_group_pairings (
    group_id UUID PRIMARY KEY,
    pairs JSONB NOT NULL CHECK (jsonb_typeof(pairs) = 'array')
);
