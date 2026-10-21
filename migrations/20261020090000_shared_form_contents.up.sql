-- Add canonical content and binding maps without reparenting existing reference nodes.
CREATE TABLE lexicon.v3_form_contents (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    content JSONB NOT NULL CHECK (jsonb_typeof(content) = 'object'),
    UNIQUE (id, entry_id)
);

CREATE TABLE lexicon.v3_form_content_bindings (
    form_id UUID PRIMARY KEY,
    entry_id UUID NOT NULL REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    content_id UUID NOT NULL,
    regional_bindings JSONB NOT NULL CHECK (jsonb_typeof(regional_bindings) = 'object'),
    FOREIGN KEY (form_id, entry_id) REFERENCES lexicon.v3_concrete_forms(id, entry_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (content_id, entry_id) REFERENCES lexicon.v3_form_contents(id, entry_id)
        DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX lexicon_v3_form_content_bindings_content_idx
    ON lexicon.v3_form_content_bindings(content_id, entry_id);

CREATE TABLE lexicon.v3_form_content_slots (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    content_id UUID NOT NULL,
    parent_slot_id UUID,
    role TEXT NOT NULL CHECK (role IN ('content', 'common', 'uk', 'us', 'pronunciation')),
    retired_at TIMESTAMPTZ,
    UNIQUE (id, entry_id, content_id),
    CHECK ((role = 'content' AND id = content_id AND parent_slot_id IS NULL)
        OR (role <> 'content' AND parent_slot_id IS NOT NULL)),
    FOREIGN KEY (parent_slot_id, entry_id, content_id)
        REFERENCES lexicon.v3_form_content_slots(id, entry_id, content_id)
        DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX lexicon_v3_form_content_slots_region_key
    ON lexicon.v3_form_content_slots(content_id, role)
    WHERE role IN ('common', 'uk', 'us');

CREATE TABLE lexicon.v3_form_content_node_slots (
    node_id UUID PRIMARY KEY,
    entry_id UUID NOT NULL REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    content_id UUID NOT NULL,
    content_slot_id UUID NOT NULL,
    FOREIGN KEY (node_id, entry_id) REFERENCES lexicon.nodes(id, entry_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (content_slot_id, entry_id, content_id)
        REFERENCES lexicon.v3_form_content_slots(id, entry_id, content_id)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE FUNCTION lexicon.v3_legacy_content_slot(kind TEXT, node_id UUID)
RETURNS UUID LANGUAGE SQL IMMUTABLE STRICT AS $$
    SELECT substring(encode(sha256(convert_to('tsz:shared-forms:' || kind || ':' || node_id::text, 'UTF8')), 'hex'), 1, 32)::uuid
$$;

CREATE FUNCTION lexicon.v3_legacy_forms_to_shared(forms JSONB)
RETURNS JSONB LANGUAGE plpgsql AS $$
DECLARE
    pos JSONB; form JSONB; grp JSONB; variant JSONB; pronunciation JSONB;
    content_id UUID; variant_slot UUID; pronunciation_slot UUID;
    contents JSONB := '[]'; positions JSONB := '[]'; bindings JSONB; groups JSONB;
    content_regions JSONB; binding_regions JSONB; content_rows JSONB; binding_rows JSONB;
    dialect TEXT; regular BOOLEAN;
BEGIN
    IF forms->>'forms_format' = 'shared_v1' THEN RETURN forms; END IF;
    FOR pos IN SELECT value FROM jsonb_array_elements(forms->'pos') LOOP
        bindings := '[]'; groups := '[]';
        FOR grp IN SELECT value FROM jsonb_array_elements(pos->'form_groups') LOOP
            groups := groups || jsonb_build_array(grp - 'dialect_rules');
        END LOOP;
        FOR form IN SELECT value FROM jsonb_array_elements(pos->'forms') LOOP
            SELECT value INTO STRICT grp FROM jsonb_array_elements(pos->'form_groups')
            WHERE EXISTS (SELECT 1 FROM jsonb_array_elements(value->'members') member WHERE member->>'form_id' = form->>'id');
            content_id := lexicon.v3_legacy_content_slot('content', (form->>'id')::uuid);
            content_regions := jsonb_build_object('mode', form->'regional_variants'->>'mode');
            binding_regions := content_regions;
            FOREACH dialect IN ARRAY ARRAY['common','uk','us'] LOOP
                variant := form->'regional_variants'->dialect;
                IF variant IS NULL THEN CONTINUE; END IF;
                variant_slot := lexicon.v3_legacy_content_slot('variant', (variant->>'id')::uuid);
                content_rows := '[]'; binding_rows := '[]';
                FOR pronunciation IN SELECT value FROM jsonb_array_elements(variant->'pronunciations') LOOP
                    pronunciation_slot := lexicon.v3_legacy_content_slot('pronunciation', (pronunciation->>'id')::uuid);
                    content_rows := content_rows || jsonb_build_array(pronunciation || jsonb_build_object('id', pronunciation_slot));
                    binding_rows := binding_rows || jsonb_build_array(jsonb_build_object('id', pronunciation->'id', 'content_slot_id', pronunciation_slot));
                END LOOP;
                regular := COALESCE((variant->>'is_regular')::boolean, (grp->>'is_regular')::boolean);
                content_regions := content_regions || jsonb_build_object(dialect, (variant - ARRAY['dialect','is_regular','component_usages']) || jsonb_build_object('id', variant_slot, 'pronunciations', content_rows));
                binding_regions := binding_regions || jsonb_build_object(dialect, jsonb_build_object('id', variant->'id', 'content_slot_id', variant_slot, 'is_regular', regular, 'pronunciations', binding_rows, 'component_usages', COALESCE(variant->'component_usages', '[]'::jsonb)));
            END LOOP;
            contents := contents || jsonb_build_array(jsonb_build_object('id', content_id, 'dialect_rules', grp->'dialect_rules', 'regional_variants', content_regions));
            bindings := bindings || jsonb_build_array(jsonb_build_object('id', form->'id', 'form_type', form->'form_type', 'content_id', content_id, 'regional_bindings', binding_regions));
        END LOOP;
        positions := positions || jsonb_build_array(pos || jsonb_build_object('forms', bindings, 'form_groups', groups));
    END LOOP;
    RETURN jsonb_build_object('forms_format', 'shared_v1', 'contents', contents, 'pos', positions);
END
$$;

-- Old immutable publications and hashes remain untouched.
UPDATE lexicon.entry_editor_projection projection
SET forms = lexicon.v3_legacy_forms_to_shared(projection.forms)
FROM lexicon.entries entry
WHERE entry.id = projection.entry_id AND entry.content_schema_version = 3;

INSERT INTO lexicon.v3_form_contents(id, entry_id, content)
SELECT (content->>'id')::uuid, projection.entry_id, content
FROM lexicon.entry_editor_projection projection, jsonb_array_elements(projection.forms->'contents') content;
INSERT INTO lexicon.v3_form_content_bindings(form_id, entry_id, content_id, regional_bindings)
SELECT (form->>'id')::uuid, projection.entry_id, (form->>'content_id')::uuid, form->'regional_bindings'
FROM lexicon.entry_editor_projection projection, jsonb_array_elements(projection.forms->'pos') pos, jsonb_array_elements(pos->'forms') form
WHERE projection.forms->>'forms_format' = 'shared_v1';
INSERT INTO lexicon.v3_form_content_slots(id, entry_id, content_id, parent_slot_id, role)
SELECT id, entry_id, id, NULL, 'content' FROM lexicon.v3_form_contents;
INSERT INTO lexicon.v3_form_content_slots(id, entry_id, content_id, parent_slot_id, role)
SELECT (region.value->>'id')::uuid, content.entry_id, content.id, content.id, region.key
FROM lexicon.v3_form_contents content, jsonb_each(content.content->'regional_variants') region
WHERE region.key IN ('common','uk','us');
INSERT INTO lexicon.v3_form_content_slots(id, entry_id, content_id, parent_slot_id, role)
SELECT (pronunciation->>'id')::uuid, content.entry_id, content.id, (region.value->>'id')::uuid, 'pronunciation'
FROM lexicon.v3_form_contents content, jsonb_each(content.content->'regional_variants') region,
LATERAL jsonb_array_elements(CASE WHEN region.key IN ('common','uk','us') THEN region.value->'pronunciations' ELSE '[]'::jsonb END) pronunciation;
INSERT INTO lexicon.v3_form_content_node_slots(node_id, entry_id, content_id, content_slot_id)
SELECT (region.value->>'id')::uuid, binding.entry_id, binding.content_id,
       (region.value->>'content_slot_id')::uuid
FROM lexicon.v3_form_content_bindings binding, jsonb_each(binding.regional_bindings) region
WHERE region.key IN ('common','uk','us');
INSERT INTO lexicon.v3_form_content_node_slots(node_id, entry_id, content_id, content_slot_id)
SELECT (pronunciation->>'id')::uuid, binding.entry_id, binding.content_id,
       (pronunciation->>'content_slot_id')::uuid
FROM lexicon.v3_form_content_bindings binding, jsonb_each(binding.regional_bindings) region,
LATERAL jsonb_array_elements(CASE WHEN region.key IN ('common','uk','us') THEN region.value->'pronunciations' ELSE '[]'::jsonb END) pronunciation;
