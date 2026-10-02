ALTER TABLE admins ADD COLUMN permission_version bigint NOT NULL DEFAULT 0 CHECK (permission_version >= 0);

CREATE TABLE admin_permission_grants (
    admin_id uuid NOT NULL REFERENCES admins(id) ON DELETE CASCADE,
    permission_key text NOT NULL,
    granted_by uuid NOT NULL REFERENCES admins(id),
    granted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (admin_id, permission_key)
);
CREATE INDEX admin_permission_grants_key_idx ON admin_permission_grants (permission_key, admin_id);

CREATE TABLE permission_tags (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (length(trim(name)) BETWEEN 1 AND 50),
    version bigint NOT NULL DEFAULT 0 CHECK (version >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX permission_tags_name_idx ON permission_tags (lower(trim(name)));

CREATE TABLE permission_tag_items (
    tag_id uuid NOT NULL REFERENCES permission_tags(id) ON DELETE CASCADE,
    permission_key text NOT NULL,
    PRIMARY KEY (tag_id, permission_key)
);
CREATE INDEX permission_tag_items_key_idx ON permission_tag_items (permission_key, tag_id);
