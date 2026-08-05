CREATE SCHEMA dictionary;

CREATE TABLE dictionary.datasets (
    id                     BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    version                TEXT NOT NULL UNIQUE,
    source_name            TEXT NOT NULL,
    source_version         TEXT NOT NULL,
    rules_version          TEXT NOT NULL,
    terms_sha256           TEXT NOT NULL,
    regions_sha256         TEXT NOT NULL,
    status                 TEXT NOT NULL
                           CHECK (status IN ('importing', 'active', 'retired', 'failed')),
    term_count             INTEGER,
    regional_surface_count INTEGER,
    evidence_count         INTEGER,
    imported_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    activated_at           TIMESTAMPTZ,

    CONSTRAINT dictionary_datasets_counts_nonnegative CHECK (
        (term_count IS NULL OR term_count >= 0)
        AND (regional_surface_count IS NULL OR regional_surface_count >= 0)
        AND (evidence_count IS NULL OR evidence_count >= 0)
    )
);

-- 版本切换时先将旧 active 改为 retired，再激活新版本；数据库保证最多一个 active。
CREATE UNIQUE INDEX dictionary_one_active_dataset_idx
ON dictionary.datasets (status)
WHERE status = 'active';

CREATE TABLE dictionary.terms (
    dataset_id                BIGINT NOT NULL
                              REFERENCES dictionary.datasets(id) ON DELETE CASCADE,
    normalized_term           TEXT NOT NULL,
    term                      TEXT NOT NULL,
    kind                      TEXT NOT NULL CHECK (kind IN ('word', 'phrase')),
    pos                       TEXT[] NOT NULL,
    status                    TEXT NOT NULL
                              CHECK (status IN ('accepted', 'accepted_with_warning')),
    warning_tags              TEXT[] NOT NULL DEFAULT '{}',
    sense_count               INTEGER NOT NULL CHECK (sense_count > 0),
    filtered_cold_sense_count INTEGER NOT NULL
                              CHECK (filtered_cold_sense_count >= 0),
    region_family             TEXT NOT NULL CHECK (region_family IN (
                                  'common_unmarked',
                                  'british_core',
                                  'american_core',
                                  'british_influenced',
                                  'american_influenced',
                                  'british_american',
                                  'mixed',
                                  'other'
                              )),
    source_regions            TEXT[] NOT NULL DEFAULT '{}',
    region_evidence_types     TEXT[] NOT NULL DEFAULT '{}',

    PRIMARY KEY (dataset_id, normalized_term),
    CONSTRAINT dictionary_terms_nonempty CHECK (
        normalized_term <> '' AND term <> '' AND cardinality(pos) > 0
    ),
    CONSTRAINT dictionary_terms_evidence_types CHECK (
        region_evidence_types <@ ARRAY['usage', 'spelling', 'alias']::TEXT[]
    )
);

-- 主键适合“已知 active dataset_id 后查词”；反向索引也支持直接按 search key 查当前版本。
CREATE INDEX dictionary_terms_lookup_idx
ON dictionary.terms (normalized_term, dataset_id);
CREATE INDEX dictionary_terms_region_family_idx
ON dictionary.terms (region_family, dataset_id);
CREATE INDEX dictionary_terms_pos_idx
ON dictionary.terms USING GIN (pos);
CREATE INDEX dictionary_terms_source_regions_idx
ON dictionary.terms USING GIN (source_regions);

CREATE TABLE dictionary.region_surfaces (
    dataset_id      BIGINT NOT NULL
                    REFERENCES dictionary.datasets(id) ON DELETE CASCADE,
    normalized_term TEXT NOT NULL,
    term            TEXT NOT NULL,
    region_family   TEXT NOT NULL CHECK (region_family IN (
                        'british_core',
                        'american_core',
                        'british_influenced',
                        'american_influenced',
                        'british_american',
                        'mixed',
                        'other'
                    )),
    families        TEXT[] NOT NULL,
    source_regions  TEXT[] NOT NULL,
    evidence_types  TEXT[] NOT NULL,
    pos             TEXT[] NOT NULL,
    targets         TEXT[] NOT NULL DEFAULT '{}',
    is_headword     BOOLEAN NOT NULL,

    PRIMARY KEY (dataset_id, normalized_term),
    CONSTRAINT dictionary_region_surfaces_nonempty CHECK (
        normalized_term <> ''
        AND term <> ''
        AND cardinality(families) > 0
        AND cardinality(source_regions) > 0
        AND cardinality(evidence_types) > 0
        AND cardinality(pos) > 0
    ),
    CONSTRAINT dictionary_region_surfaces_families CHECK (
        families <@ ARRAY[
            'british_core',
            'american_core',
            'british_influenced',
            'american_influenced',
            'mixed',
            'other'
        ]::TEXT[]
    ),
    CONSTRAINT dictionary_region_surfaces_evidence_types CHECK (
        evidence_types <@ ARRAY['usage', 'spelling', 'alias']::TEXT[]
    )
);

CREATE INDEX dictionary_region_surfaces_lookup_idx
ON dictionary.region_surfaces (normalized_term, dataset_id);
CREATE INDEX dictionary_region_surfaces_family_idx
ON dictionary.region_surfaces (region_family, dataset_id);
CREATE INDEX dictionary_region_surfaces_regions_idx
ON dictionary.region_surfaces USING GIN (source_regions);
CREATE INDEX dictionary_region_surfaces_targets_idx
ON dictionary.region_surfaces USING GIN (targets);

-- 明细表不做家族化覆盖：original_region_tags 和 raw_tags 原样保留来源证据。
CREATE TABLE dictionary.region_evidence (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    dataset_id           BIGINT NOT NULL,
    normalized_term      TEXT NOT NULL,
    evidence_type        TEXT NOT NULL
                         CHECK (evidence_type IN ('usage', 'spelling', 'alias')),
    original_region_tags TEXT[] NOT NULL,
    raw_tags             TEXT[] NOT NULL,
    pos                  TEXT NOT NULL,
    targets              TEXT[] NOT NULL DEFAULT '{}',

    FOREIGN KEY (dataset_id, normalized_term)
        REFERENCES dictionary.region_surfaces(dataset_id, normalized_term)
        ON DELETE CASCADE,
    CONSTRAINT dictionary_region_evidence_nonempty CHECK (
        cardinality(original_region_tags) > 0
        AND cardinality(raw_tags) > 0
        AND pos <> ''
    )
);

CREATE INDEX dictionary_region_evidence_surface_idx
ON dictionary.region_evidence (dataset_id, normalized_term);
CREATE INDEX dictionary_region_evidence_type_idx
ON dictionary.region_evidence (evidence_type, dataset_id);
CREATE INDEX dictionary_region_evidence_original_regions_idx
ON dictionary.region_evidence USING GIN (original_region_tags);

CREATE VIEW dictionary.active_terms AS
SELECT terms.*
FROM dictionary.terms
JOIN dictionary.datasets ON datasets.id = terms.dataset_id
WHERE datasets.status = 'active';

CREATE VIEW dictionary.active_region_surfaces AS
SELECT region_surfaces.*
FROM dictionary.region_surfaces
JOIN dictionary.datasets ON datasets.id = region_surfaces.dataset_id
WHERE datasets.status = 'active';
