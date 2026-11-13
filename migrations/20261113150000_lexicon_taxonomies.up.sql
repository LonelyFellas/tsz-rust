-- 独立配置目录；本次不引入词条/例句引用，也不迁移或改写现有内容。
CREATE TABLE catalog.lexicon_taxonomies (
    id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('registers', 'domains')),
    code text NOT NULL CHECK (code ~ '^[a-z][a-z0-9_]{0,31}$'),
    name_zh text NOT NULL CHECK (char_length(name_zh) BETWEEN 1 AND 64 AND name_zh = btrim(name_zh)),
    name_en text NOT NULL CHECK (char_length(name_en) BETWEEN 1 AND 64 AND name_en = btrim(name_en)),
    sort_order integer NOT NULL DEFAULT 0,
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_by_admin_id uuid REFERENCES admins(id),
    updated_by_admin_id uuid REFERENCES admins(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT lexicon_taxonomies_code_key UNIQUE (kind, code)
);
CREATE UNIQUE INDEX lexicon_taxonomies_name_zh_key ON catalog.lexicon_taxonomies (kind, lower(name_zh));
CREATE UNIQUE INDEX lexicon_taxonomies_name_en_key ON catalog.lexicon_taxonomies (kind, lower(name_en));
CREATE INDEX lexicon_taxonomies_order ON catalog.lexicon_taxonomies (kind, sort_order, created_at, id);
