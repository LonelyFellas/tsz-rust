-- V2 词条内容格式下线：库里只允许 content_schema_version = 3。
--
-- 三块：①拒绝残留 V2 数据（有就说明不该跑这条迁移，宁可失败）②删掉只服务 V2 的存储表与
-- V2→V3 迁移账本 ③收紧 entries / v3_entry_state 的版本与来源约束。
DO $$
DECLARE
    legacy_entries BIGINT;
    legacy_publications BIGINT;
    migrated_states BIGINT;
BEGIN
    SELECT count(*) INTO legacy_entries
    FROM lexicon.entries WHERE content_schema_version <> 3;
    SELECT count(*) INTO legacy_publications
    FROM lexicon.entry_publications WHERE content_schema_version <> 3;
    SELECT count(*) INTO migrated_states
    FROM lexicon.v3_entry_state WHERE origin <> 'native';

    IF legacy_entries > 0 OR legacy_publications > 0 OR migrated_states > 0 THEN
        RAISE EXCEPTION
            'cannot drop the V2 content schema while legacy rows remain: % entries, % publications, % migrated states',
            legacy_entries, legacy_publications, migrated_states;
    END IF;
END
$$;

-- V2 词形与主词的规范化存储；V3 换成了 v3_* 一族，这些表在 V3 下恒空。
DROP TABLE lexicon.pronunciations;
DROP TABLE lexicon.form_variants;
DROP TABLE lexicon.form_slots;
DROP TABLE lexicon.form_groups;
DROP TABLE lexicon.entry_headword_keys;
DROP TABLE lexicon.entry_headwords;

-- V2→V3 迁移账本：迁移工具链已随本次改动整体下线。
DROP TABLE lexicon.v3_migration_map;
DROP TABLE lexicon.v3_migration_entries;
DROP TABLE lexicon.v3_migration_batches;

-- entries 只留 V3：默认值、check、以及只有 V2 才写的主词形状列。
ALTER TABLE lexicon.entries
    DROP CONSTRAINT lexicon_entries_versioned_headword_shape_check,
    DROP CONSTRAINT lexicon_entries_headword_mode_check,
    DROP CONSTRAINT lexicon_entries_source_dialect_check,
    DROP CONSTRAINT lexicon_entries_schema_version_check,
    DROP CONSTRAINT lexicon_entries_schema_kind_check,
    DROP COLUMN headword_mode,
    DROP COLUMN source_dialect,
    ALTER COLUMN content_schema_version SET DEFAULT 3,
    ADD CONSTRAINT lexicon_entries_schema_version_check
        CHECK (content_schema_version = 3),
    ADD CONSTRAINT lexicon_entries_schema_kind_check
        CHECK (kind = ANY (ARRAY['word'::text, 'phrase'::text]));

-- v3_entry_state：origin 只剩 native，迁移来源列随账本一起退场。
ALTER TABLE lexicon.v3_entry_state
    DROP CONSTRAINT lexicon_v3_entry_state_origin_shape_check,
    DROP CONSTRAINT lexicon_v3_entry_state_origin_check,
    DROP CONSTRAINT lexicon_v3_entry_state_source_revision_check,
    DROP CONSTRAINT lexicon_v3_entry_state_source_publication_fkey,
    DROP COLUMN migration_batch_id,
    DROP COLUMN source_publication_id,
    DROP COLUMN source_revision,
    DROP COLUMN publication_canary_enabled,
    ADD CONSTRAINT lexicon_v3_entry_state_origin_check
        CHECK (origin = 'native'::text);

-- 两张按 entries 版本绑定的表：默认值和 check 跟着收到 3，免得裸 INSERT 靠旧默认值写出 2
-- 再被复合外键拒掉。source_shape_check 的 V2 那一半同时退场。
ALTER TABLE lexicon.entry_pos
    DROP CONSTRAINT lexicon_entry_pos_schema_version_check,
    DROP CONSTRAINT lexicon_entry_pos_versioned_modes_check,
    ALTER COLUMN content_schema_version SET DEFAULT 3,
    ADD CONSTRAINT lexicon_entry_pos_schema_version_check
        CHECK (content_schema_version = 3),
    ADD CONSTRAINT lexicon_entry_pos_versioned_modes_check
        CHECK (
            spelling_mode IS NOT NULL
            AND phonetic_mode IS NOT NULL
            AND (spelling_mode <> 'distinguish'::text OR phonetic_mode = 'distinguish'::text)
        );

ALTER TABLE lexicon.surface_sources
    DROP CONSTRAINT lexicon_surface_sources_schema_version_check,
    DROP CONSTRAINT lexicon_surface_sources_source_shape_check,
    ALTER COLUMN content_schema_version SET DEFAULT 3,
    ADD CONSTRAINT lexicon_surface_sources_schema_version_check
        CHECK (content_schema_version = 3),
    ADD CONSTRAINT lexicon_surface_sources_source_shape_check
        CHECK (
            source_kind = 'form_variant'::text
            AND source_node_id IS NOT NULL
            AND source_node_id = variant_id
            AND pos_id IS NOT NULL
            AND pos IS NOT NULL
            AND btrim(pos) <> ''::text
            AND form_id IS NOT NULL
            AND variant_id IS NOT NULL
            AND group_ids IS NOT NULL
            AND cardinality(group_ids) > 0
            AND array_position(group_ids, NULL::uuid) IS NULL
            AND projection_version IS NOT NULL
            AND projection_version = btrim(projection_version)
            AND char_length(projection_version) >= 1
            AND char_length(projection_version) <= 100
            AND form_type IS NOT NULL
            AND form_type ~ '^[a-z][a-z0-9_]{0,31}$'::text
            AND (
                dialect = 'common'::text AND (dialect_scope = ANY (ARRAY['uk'::text, 'us'::text]))
                OR dialect = 'uk'::text AND dialect_scope = 'uk'::text
                OR dialect = 'us'::text AND dialect_scope = 'us'::text
            )
        );
