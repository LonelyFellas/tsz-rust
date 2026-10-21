CREATE TABLE speech.dubbing_groups (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    groups JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(groups) = 'array'),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    updated_by UUID REFERENCES admins(id),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO speech.dubbing_groups(singleton) VALUES(TRUE);
