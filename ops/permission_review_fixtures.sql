BEGIN;
DO $$
BEGIN
    IF current_database() <> 'permission_test' THEN
        RAISE EXCEPTION 'review fixtures require the task-isolated permission_test database';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM admins WHERE id = '22222222-2222-4222-8222-222222222222' AND phone = '13800139002') THEN
        RAISE EXCEPTION 'isolated acceptance administrator is missing';
    END IF;
END $$;

INSERT INTO lexicon.shared_sentences (id, content, create_digest, source_entry_id, created_by_admin_id)
VALUES (
    '66666666-6666-4666-8666-666666666666',
    '{"id":"66666666-6666-4666-8666-666666666666","level":"B1","en_text":{"mode":"unified","common":{"id":"77777777-7777-4777-8777-777777777777","origin":"manual","value":{"version":2,"text":"This is a review test.","annotations":[]}}},"zh_text_id":"88888888-8888-4888-8888-888888888888","zh_text":{"version":2,"text":"这是权限修复测试。","annotations":[]},"zh_translations":[{"id":"88888888-8888-4888-8888-888888888888","band":"balanced_fluency","language":"zh","content":{"version":2,"text":"这是权限修复测试。","annotations":[]}}],"links":[]}'::jsonb,
    'permission-review-isolated-fixture',
    '01a0f65a-43d8-7912-aacb-db34944b938e',
    '22222222-2222-4222-8222-222222222222'
) ON CONFLICT (id) DO NOTHING;
COMMIT;
