BEGIN;
DO $$
BEGIN
    IF current_database() NOT IN ('permission_test', 'permission_legacy_test') THEN
        RAISE EXCEPTION 'permission acceptance fixtures require the task-isolated database';
    END IF;
END $$;

INSERT INTO admins (id, phone, display_name, password_hash, role, status, must_change_password, created_by_admin_id)
SELECT fixture.id, fixture.phone, fixture.name, actor.password_hash, 'admin', 'active', false, actor.id
FROM (VALUES
    ('11111111-1111-4111-8111-111111111111'::uuid, '13800139001', '权限验收甲'),
    ('22222222-2222-4222-8222-222222222222'::uuid, '13800139002', '权限验收乙')
) AS fixture(id, phone, name)
JOIN admins actor ON actor.phone = '13800139000' AND actor.role = 'super_admin'
ON CONFLICT (id) DO NOTHING;

INSERT INTO users (id, phone, email, password_hash, display_name, avatar_url)
SELECT '33333333-3333-4333-8333-333333333333'::uuid, '13800139003', 'permission-user@example.test', password_hash, '权限验收学习者', ''
FROM admins WHERE phone = '13800139000' AND role = 'super_admin'
ON CONFLICT (id) DO NOTHING;
INSERT INTO user_roles (user_id, role)
SELECT '33333333-3333-4333-8333-333333333333'::uuid, 'student'
WHERE EXISTS (SELECT 1 FROM users WHERE id = '33333333-3333-4333-8333-333333333333')
ON CONFLICT DO NOTHING;

INSERT INTO dictionary.datasets (version, source_name, source_version, rules_version, terms_sha256, regions_sha256, status)
SELECT 'permission-acceptance-v1', 'isolated-fixture', 'v1', 'v1', 'fixture-terms', 'fixture-regions', 'active'
WHERE NOT EXISTS (SELECT 1 FROM dictionary.datasets WHERE status = 'active')
ON CONFLICT DO NOTHING;
INSERT INTO dictionary.terms (dataset_id, normalized_term, term, kind, pos, status, sense_count, filtered_cold_sense_count, region_family)
SELECT dataset.id, term.surface, term.surface, 'word', ARRAY['noun'], 'accepted', 1, 0, 'common_unmarked'
FROM dictionary.datasets dataset CROSS JOIN (VALUES ('permissionalpha'), ('permissionbeta')) AS term(surface)
WHERE dataset.status = 'active'
ON CONFLICT DO NOTHING;
COMMIT;
