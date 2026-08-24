-- 例句里「句中某个词指向别的词条某个词义」的关联，由发布时的自动解析产出，
-- 管理员可事后修正。
--
-- 刻意不挂进 lexicon.sentences 的子表体系：内容表每次保存词义步都会整表重写
-- （repository/entries.rs 的 replace_entry_content → delete_current_content），
-- 而按产品规则草稿保存根本不带关联字段，挂进去就会被下一次保存删掉。
-- 锚点因此用 lexicon.nodes——节点 ID 跨保存稳定。
CREATE TABLE lexicon.sentence_associations (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    sentence_id UUID NOT NULL,
    -- 位置落在 en_text 的哪一侧：distinguish 例句的 uk/us 两份正文下标会错位。
    source_dialect TEXT NOT NULL
        CONSTRAINT lexicon_sentence_associations_dialect_check
        CHECK (source_dialect IN ('common', 'uk', 'us')),
    -- RichText.text 的 Unicode 码点下标，左闭右开，与 spans/liaisons 同一口径。
    range_start INTEGER NOT NULL
        CONSTRAINT lexicon_sentence_associations_range_start_check CHECK (range_start >= 0),
    range_end INTEGER NOT NULL
        CONSTRAINT lexicon_sentence_associations_range_end_check CHECK (range_end > range_start),
    surface TEXT NOT NULL
        CONSTRAINT lexicon_sentence_associations_surface_check CHECK (
            surface = btrim(surface) AND char_length(surface) BETWEEN 1 AND 200
        ),
    target_entry_id UUID NOT NULL,
    target_sense_id UUID NOT NULL,
    -- 命中目标词条的哪个词形槽位；人工关联可能落在词库没录的词形上，故可空。
    target_form_slot_id UUID,
    origin TEXT NOT NULL
        CONSTRAINT lexicon_sentence_associations_origin_check CHECK (origin IN ('auto', 'manual')),
    -- distinguish 词条的词头快照是两侧拼起来的 `uk / us`，每侧上限 200 码点，
    -- 加分隔符最长 403——上限必须留在它之上，否则极端词头会让整个发布以 500 回滚。
    target_headword_snapshot TEXT NOT NULL
        CONSTRAINT lexicon_sentence_associations_headword_length_check
        CHECK (char_length(target_headword_snapshot) <= 500),
    target_gloss_snapshot TEXT NOT NULL
        CONSTRAINT lexicon_sentence_associations_gloss_length_check
        CHECK (char_length(target_gloss_snapshot) <= 5000),
    resolved_pos TEXT NOT NULL
        CONSTRAINT lexicon_sentence_associations_pos_check CHECK (
            resolved_pos = btrim(resolved_pos) AND char_length(resolved_pos) BETWEEN 1 AND 32
        ),
    resolved_form_type TEXT
        CONSTRAINT lexicon_sentence_associations_form_type_check CHECK (
            resolved_form_type IN (
                'base', 'present_participle', 'past_tense', 'past_participle',
                'third_person_singular', 'plural', 'comparative', 'superlative'
            )
        ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT lexicon_sentence_associations_sentence_fkey
        FOREIGN KEY (sentence_id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_sentence_associations_target_fkey
        FOREIGN KEY (target_sense_id, target_entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_sentence_associations_target_slot_fkey
        FOREIGN KEY (target_form_slot_id, target_entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_sentence_associations_external_check CHECK (entry_id <> target_entry_id),
    CONSTRAINT lexicon_sentence_associations_form_shape_check CHECK (
        (target_form_slot_id IS NULL) = (resolved_form_type IS NULL)
    ),
    -- 一个起始位置最多一条关联；顺带挡住并发重复插入。
    CONSTRAINT lexicon_sentence_associations_position_key
        UNIQUE (sentence_id, source_dialect, range_start)
);

CREATE INDEX lexicon_sentence_associations_entry_idx
    ON lexicon.sentence_associations (entry_id, sentence_id, source_dialect, range_start);
CREATE INDEX lexicon_sentence_associations_target_idx
    ON lexicon.sentence_associations (target_entry_id, target_sense_id);

-- 「这一侧正文解析到哪个版本了」。
--
-- 没有它就分不清「解析过、句中没有可关联的词」与「还没解析过」，也无法在正文没变时
-- 跳过重算——而跳过重算正是管理员的事后修正能活过下次发布的唯一原因。
CREATE TABLE lexicon.sentence_association_scans (
    sentence_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    source_dialect TEXT NOT NULL
        CONSTRAINT lexicon_sentence_association_scans_dialect_check
        CHECK (source_dialect IN ('common', 'uk', 'us')),
    text_hash BYTEA NOT NULL,
    -- 切词、停用词、筛选与歧义口径的版本；一升，各例句在下次发布时自然重算。
    resolver_version SMALLINT NOT NULL
        CONSTRAINT lexicon_sentence_association_scans_version_check CHECK (resolver_version > 0),
    scanned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (sentence_id, source_dialect),
    CONSTRAINT lexicon_sentence_association_scans_sentence_fkey
        FOREIGN KEY (sentence_id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE
);

CREATE INDEX lexicon_sentence_association_scans_entry_idx
    ON lexicon.sentence_association_scans (entry_id, sentence_id);
