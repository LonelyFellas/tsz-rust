CREATE TABLE catalog.form_sharing_settings (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    updated_by_admin_id UUID REFERENCES admins(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO catalog.form_sharing_settings DEFAULT VALUES;

CREATE TABLE catalog.form_sharing_groups (
    id UUID PRIMARY KEY,
    sort_order INTEGER NOT NULL UNIQUE
);
CREATE TABLE catalog.form_sharing_members (
    group_id UUID NOT NULL REFERENCES catalog.form_sharing_groups(id) ON DELETE CASCADE,
    part_of_speech_id UUID NOT NULL REFERENCES catalog.parts_of_speech(id) ON DELETE RESTRICT,
    form_type_id UUID NOT NULL REFERENCES catalog.form_types(id) ON DELETE RESTRICT,
    sort_order INTEGER NOT NULL,
    PRIMARY KEY (part_of_speech_id, form_type_id),
    UNIQUE (group_id, sort_order)
);

-- Only seed known catalog identities. Missing/custom categories remain manually configurable.
INSERT INTO catalog.form_sharing_groups VALUES ('019f1650-0000-7000-8000-000000000001', 0);
INSERT INTO catalog.form_sharing_members
SELECT '019f1650-0000-7000-8000-000000000001', p.id, f.id,
       row_number() OVER (ORDER BY p.sort_order,p.created_at,p.id)
FROM catalog.parts_of_speech p CROSS JOIN catalog.form_types f
WHERE p.kind='word' AND f.code='base';

INSERT INTO catalog.form_sharing_groups VALUES ('019f1650-0000-7000-8000-000000000002', 1);
INSERT INTO catalog.form_sharing_members
SELECT '019f1650-0000-7000-8000-000000000002', p.id, f.id,
       row_number() OVER (ORDER BY p.sort_order,p.id,f.id)
FROM catalog.parts_of_speech p JOIN catalog.form_types f ON f.part_of_speech_id=p.id
WHERE (p.code='noun' AND f.code='plural') OR (p.code='verb' AND f.code='third_person_singular');

-- A shared template needs at least two labels; do not invent missing catalog entries.
DELETE FROM catalog.form_sharing_groups g WHERE
    (SELECT count(*) FROM catalog.form_sharing_members m WHERE m.group_id=g.id)<2;
UPDATE catalog.metadata SET version=version+1,updated_at=now() WHERE id=TRUE;

CREATE VIEW catalog.form_sharing_template AS
SELECT s.revision, COALESCE((SELECT jsonb_agg(jsonb_build_object('id',g.id,'members',
    (SELECT jsonb_agg(jsonb_build_object('part_of_speech_id',m.part_of_speech_id,'form_type_id',m.form_type_id) ORDER BY m.sort_order)
     FROM catalog.form_sharing_members m WHERE m.group_id=g.id)) ORDER BY g.sort_order)
    FROM catalog.form_sharing_groups g),'[]'::jsonb) AS groups
FROM catalog.form_sharing_settings s WHERE s.id=TRUE;
