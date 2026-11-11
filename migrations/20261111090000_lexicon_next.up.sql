-- Independent identities; no old lexicon/dictionary data is copied or referenced.
CREATE SCHEMA lexicon_next;
CREATE TABLE lexicon_next.entries (
    id uuid PRIMARY KEY,
    target_language text NOT NULL CHECK (target_language ~ '^[a-z]{2,3}$'),
    kind text NOT NULL CHECK (kind IN ('word', 'phrase')),
    headword text NOT NULL CHECK (char_length(headword) BETWEEN 1 AND 200),
    owner_id uuid NOT NULL REFERENCES admins(id),
    revision bigint NOT NULL CHECK (revision > 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);
-- Retain node identities after removal, preventing reuse across entries, kinds or senses.
CREATE TABLE lexicon_next.nodes (
    id uuid PRIMARY KEY,
    entry_id uuid NOT NULL REFERENCES lexicon_next.entries(id),
    kind text NOT NULL CHECK (kind IN ('form', 'sense', 'definition', 'equivalent')),
    parent_id uuid,
    UNIQUE (id, entry_id),
    FOREIGN KEY (parent_id, entry_id) REFERENCES lexicon_next.nodes(id, entry_id)
);
CREATE TABLE lexicon_next.forms (
    id uuid PRIMARY KEY REFERENCES lexicon_next.nodes(id),
    locale text NOT NULL,
    text text NOT NULL CHECK (char_length(text) BETWEEN 1 AND 200),
    sort_order integer NOT NULL
);
CREATE TABLE lexicon_next.senses (
    id uuid PRIMARY KEY REFERENCES lexicon_next.nodes(id),
    part_of_speech text,
    sort_order integer NOT NULL
);
CREATE TABLE lexicon_next.definitions (
    id uuid PRIMARY KEY REFERENCES lexicon_next.nodes(id),
    sense_id uuid NOT NULL REFERENCES lexicon_next.senses(id),
    locale text NOT NULL,
    text text NOT NULL CHECK (char_length(text) BETWEEN 1 AND 5000),
    style text NOT NULL CHECK (style IN ('definition', 'sentence')),
    sort_order integer NOT NULL
);
CREATE TABLE lexicon_next.equivalents (
    id uuid PRIMARY KEY REFERENCES lexicon_next.nodes(id),
    sense_id uuid NOT NULL REFERENCES lexicon_next.senses(id),
    locale text NOT NULL,
    text text NOT NULL CHECK (char_length(text) BETWEEN 1 AND 200),
    sort_order integer NOT NULL
);
CREATE TABLE lexicon_next.draft_revisions (
    entry_id uuid NOT NULL REFERENCES lexicon_next.entries(id),
    revision bigint NOT NULL CHECK (revision > 0),
    actor_id uuid NOT NULL REFERENCES admins(id),
    document jsonb NOT NULL CHECK (jsonb_typeof(document) = 'object' AND document->>'schema_version' = '1'),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (entry_id, revision)
);
CREATE INDEX ON lexicon_next.nodes(entry_id);
