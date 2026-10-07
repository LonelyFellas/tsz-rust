ALTER TABLE permission_tags
    ADD COLUMN color text NOT NULL DEFAULT 'default'
    CHECK (color IN ('default', 'blue', 'cyan', 'green', 'gold', 'orange', 'red', 'purple'));
