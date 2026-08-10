-- Add up migration script here
CREATE SCHEMA catalog;

CREATE TABLE catalog.metadata (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id IS TRUE),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO catalog.metadata (id, version)
VALUES (TRUE, 1);
