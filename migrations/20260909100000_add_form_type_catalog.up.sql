CREATE TABLE catalog.form_types (
    id UUID PRIMARY KEY,
    code TEXT NOT NULL UNIQUE CHECK (code ~ '^[a-z][a-z0-9_]{0,31}$'),
    name_zh TEXT NOT NULL CHECK (name_zh = btrim(name_zh) AND char_length(name_zh) BETWEEN 1 AND 64),
    name_en TEXT NOT NULL CHECK (name_en = btrim(name_en) AND char_length(name_en) BETWEEN 1 AND 64),
    short_name_zh TEXT NOT NULL CHECK (short_name_zh = btrim(short_name_zh) AND char_length(short_name_zh) BETWEEN 1 AND 16),
    abbreviation TEXT NOT NULL CHECK (abbreviation = btrim(abbreviation) AND char_length(abbreviation) BETWEEN 1 AND 16),
    full_name_en TEXT NOT NULL CHECK (full_name_en = btrim(full_name_en) AND char_length(full_name_en) BETWEEN 1 AND 64),
    sort_order INTEGER NOT NULL DEFAULT 100,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_by_admin_id UUID REFERENCES admins(id) ON DELETE RESTRICT,
    updated_by_admin_id UUID REFERENCES admins(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX catalog_form_types_name_zh_unique_idx ON catalog.form_types (name_zh);
CREATE UNIQUE INDEX catalog_form_types_name_en_unique_idx ON catalog.form_types (lower(name_en));
CREATE UNIQUE INDEX catalog_form_types_short_name_zh_unique_idx ON catalog.form_types (short_name_zh);
CREATE UNIQUE INDEX catalog_form_types_abbreviation_unique_idx ON catalog.form_types (lower(abbreviation));
CREATE UNIQUE INDEX catalog_form_types_full_name_en_unique_idx ON catalog.form_types (lower(full_name_en));
CREATE INDEX catalog_form_types_order_idx ON catalog.form_types(sort_order, created_at, id);
INSERT INTO catalog.form_types(id,code,name_zh,name_en,short_name_zh,abbreviation,full_name_en,sort_order) VALUES ('019f1000-0000-7000-8000-000000000001','base','原形','Base form','原形','base','base form',0);
INSERT INTO catalog.form_types(id,code,name_zh,name_en,short_name_zh,abbreviation,full_name_en,sort_order) VALUES ('019f1000-0000-7000-8000-000000000002','third_person_singular','第三人称单数','Third person singular','第三人称单数','3sg','third person singular',10);
INSERT INTO catalog.form_types(id,code,name_zh,name_en,short_name_zh,abbreviation,full_name_en,sort_order) VALUES ('019f1000-0000-7000-8000-000000000003','present_participle','现在分词','Present participle','现在分词','pres.part.','present participle',20);
INSERT INTO catalog.form_types(id,code,name_zh,name_en,short_name_zh,abbreviation,full_name_en,sort_order) VALUES ('019f1000-0000-7000-8000-000000000004','past_tense','过去式','Past tense','过去式','past','past tense',30);
INSERT INTO catalog.form_types(id,code,name_zh,name_en,short_name_zh,abbreviation,full_name_en,sort_order) VALUES ('019f1000-0000-7000-8000-000000000005','past_participle','过去分词','Past participle','过去分词','past.part.','past participle',40);
INSERT INTO catalog.form_types(id,code,name_zh,name_en,short_name_zh,abbreviation,full_name_en,sort_order) VALUES ('019f1000-0000-7000-8000-000000000006','plural','复数','Plural','复数','pl.','plural',50);
INSERT INTO catalog.form_types(id,code,name_zh,name_en,short_name_zh,abbreviation,full_name_en,sort_order) VALUES ('019f1000-0000-7000-8000-000000000007','comparative','比较级','Comparative','比较级','comp.','comparative',60);
INSERT INTO catalog.form_types(id,code,name_zh,name_en,short_name_zh,abbreviation,full_name_en,sort_order) VALUES ('019f1000-0000-7000-8000-000000000008','superlative','最高级','Superlative','最高级','superl.','superlative',70);

CREATE FUNCTION catalog.protect_form_type_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' AND OLD.code = 'base' THEN
        RAISE EXCEPTION 'base form type cannot be deleted' USING ERRCODE = '23514', CONSTRAINT = 'catalog_form_types_base_required';
    END IF;
    IF TG_OP = 'UPDATE' AND OLD.code <> NEW.code THEN
        RAISE EXCEPTION 'form type code cannot be changed' USING ERRCODE = '23514', CONSTRAINT = 'catalog_form_types_code_immutable';
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER catalog_form_types_identity BEFORE UPDATE OR DELETE ON catalog.form_types
    FOR EACH ROW EXECUTE FUNCTION catalog.protect_form_type_identity();

CREATE TABLE lexicon.entry_publication_form_type_refs (
    publication_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    form_type TEXT NOT NULL REFERENCES catalog.form_types(code) ON DELETE RESTRICT,
    PRIMARY KEY(publication_id, form_type),
    FOREIGN KEY(publication_id, entry_id) REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE CASCADE
);
CREATE INDEX lexicon_publication_form_type_refs_catalog_idx ON lexicon.entry_publication_form_type_refs(form_type, entry_id);
-- Preserve all retained publication versions, including types used only in historical snapshots.
INSERT INTO lexicon.entry_publication_form_type_refs(publication_id, entry_id, form_type)
SELECT DISTINCT p.id, p.entry_id, value #>> '{}'
FROM lexicon.entry_publications p
CROSS JOIN LATERAL (
    SELECT value FROM jsonb_path_query(p.snapshot, '$.forms.pos[*].**.form_type') value
    UNION SELECT value FROM jsonb_path_query(p.snapshot, '$.forms.pos[*].**.target_form_type') value
    UNION SELECT value FROM jsonb_path_query(p.snapshot, '$.meanings.pos[*].**.target_form_type') value
) referenced(value);

ALTER TABLE lexicon.v3_concrete_forms DROP CONSTRAINT lexicon_v3_concrete_forms_type_check, ADD CONSTRAINT lexicon_v3_concrete_forms_type_check CHECK (form_type ~ '^[a-z][a-z0-9_]{0,31}$');
ALTER TABLE lexicon.v3_concrete_forms ADD CONSTRAINT v3_concrete_forms_form_type_catalog_fkey FOREIGN KEY(form_type) REFERENCES catalog.form_types(code) ON DELETE RESTRICT;
CREATE INDEX v3_concrete_forms_form_type_idx ON lexicon.v3_concrete_forms(form_type, entry_id);

ALTER TABLE lexicon.form_slots DROP CONSTRAINT lexicon_form_slots_type_check, ADD CONSTRAINT lexicon_form_slots_type_check CHECK (form_type ~ '^[a-z][a-z0-9_]{0,31}$');
ALTER TABLE lexicon.form_slots ADD CONSTRAINT form_slots_form_type_catalog_fkey FOREIGN KEY(form_type) REFERENCES catalog.form_types(code) ON DELETE RESTRICT;
CREATE INDEX form_slots_form_type_idx ON lexicon.form_slots(form_type, entry_id);

ALTER TABLE lexicon.surface_sources DROP CONSTRAINT lexicon_surface_sources_source_shape_check, ADD CONSTRAINT lexicon_surface_sources_source_shape_check CHECK (
        (
            content_schema_version = 2
            AND source_kind IN ('headword', 'form')
            AND form_id IS NULL
            AND variant_id IS NULL
            AND group_ids IS NULL
            AND projection_version IS NULL
            AND (
                (
                    source_kind = 'headword'
                    AND source_node_id IS NULL
                    AND pos_id IS NULL
                    AND pos IS NULL
                    AND form_type IS NULL
                )
                OR
                (
                    source_kind = 'form'
                    AND source_node_id IS NOT NULL
                    AND pos_id IS NOT NULL
                    AND pos IS NOT NULL
                    AND btrim(pos) <> ''
                    AND form_type IS NOT NULL
                    AND form_type ~ '^[a-z][a-z0-9_]{0,31}$'
                )
            )
        )
        OR
        (
            content_schema_version = 3
            AND source_kind = 'form_variant'
            AND source_node_id IS NOT NULL
            AND source_node_id = variant_id
            AND pos_id IS NOT NULL
            AND pos IS NOT NULL
            AND btrim(pos) <> ''
            AND form_id IS NOT NULL
            AND variant_id IS NOT NULL
            AND group_ids IS NOT NULL
            AND cardinality(group_ids) > 0
            AND array_position(group_ids, NULL) IS NULL
            AND projection_version IS NOT NULL
            AND projection_version = btrim(projection_version)
            AND char_length(projection_version) BETWEEN 1 AND 100
            AND form_type IS NOT NULL
            AND form_type ~ '^[a-z][a-z0-9_]{0,31}$'
            AND (
                (dialect = 'common' AND dialect_scope IN ('uk', 'us'))
                OR (dialect = 'uk' AND dialect_scope = 'uk')
                OR (dialect = 'us' AND dialect_scope = 'us')
            )
        )
    );

ALTER TABLE lexicon.sentence_associations DROP CONSTRAINT lexicon_sentence_associations_form_type_check, ADD CONSTRAINT lexicon_sentence_associations_form_type_check CHECK (
            resolved_form_type ~ '^[a-z][a-z0-9_]{0,31}$'
        );

ALTER TABLE lexicon.v3_phrase_variant_component_usages DROP CONSTRAINT lexicon_v3_phrase_components_target_form_type_check, ADD CONSTRAINT lexicon_v3_phrase_components_target_form_type_check CHECK (target_form_type ~ '^[a-z][a-z0-9_]{0,31}$');

ALTER TABLE lexicon.v3_phrase_sense_component_usages DROP CONSTRAINT lexicon_v3_phrase_components_target_form_type_check, ADD CONSTRAINT lexicon_v3_phrase_components_target_form_type_check CHECK (target_form_type ~ '^[a-z][a-z0-9_]{0,31}$');
UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
