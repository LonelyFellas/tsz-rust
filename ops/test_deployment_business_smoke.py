import copy
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import Mock
import uuid

sys.path.insert(0, str(Path(__file__).parent))

import deployment_business_smoke as business
from deployment_auth_smoke import SmokeFailure


class BusinessSmokeTests(unittest.TestCase):
    def setUp(self):
        self.resource = business.resources(str(uuid.uuid4()))

    def test_fixture_is_exactly_derived_and_collision_does_not_insert(self):
        sql = Mock(return_value='["already-exists"]')
        with self.assertRaisesRegex(SmokeFailure, "identity_exists"):
            business.seed(sql, self.resource)
        self.assertEqual(sql.call_count, 1)
        invalid = {**self.resource, "user_id": str(uuid.uuid4())}
        sql.reset_mock()
        with self.assertRaisesRegex(SmokeFailure, "invalid_fixture_identity"):
            business.cleanup(sql, invalid)
        sql.assert_not_called()

    def test_cleanup_records_ids_before_delete_and_reaudits_same_ids(self):
        identifier = str(uuid.uuid4())
        sql = Mock(side_effect=[json.dumps([identifier]), '', 'true'])
        order = []
        def record(ids):
            self.assertEqual(sql.call_count, 1)
            order.append(ids)
        business.cleanup(sql, self.resource, record_ids=record)
        self.assertEqual(order, [[identifier]])
        deletion = sql.call_args_list[1].args[0]
        audit = sql.call_args_list[2].args[0]
        self.assertIn(identifier, deletion)
        self.assertIn(identifier, audit)
        self.assertIn("FOR UPDATE", deletion)
        self.assertIn("pg_advisory_xact_lock", deletion)
        self.assertIn("BEGIN READ ONLY", audit)
        self.assertIn('pg_constraint', deletion)
        self.assertIn("('public','wordlists','owner_user_id')", deletion)
        self.assertNotIn("('public','avatar_uploads','user_id')", deletion)
        self.assertNotIn("('public','teacher_applications','user_id')", deletion)
        for table in ("wordlist_audio_jobs", "coin_wallets", "wordlist_tips", "learning_questions", "learning_memory_sources", "platform.outbox_events", "audit.admin_actions"):
            self.assertIn(table, deletion)
            self.assertIn(table, audit)
        # A prior SQL commit removed the row; retry still audits the persisted exact ID.
        sql = Mock(side_effect=['[]', '', 'true'])
        business.cleanup(sql, self.resource, known_ids=[identifier])
        self.assertIn(identifier, sql.call_args_list[2].args[0])

    def test_cleanup_failure_or_residual_cannot_report_success(self):
        for outputs in (['[]', RuntimeError('guard refused')], ['[]', '', 'false']):
            with self.subTest(outputs=outputs), self.assertRaises((RuntimeError, SmokeFailure)):
                business.cleanup(Mock(side_effect=outputs), self.resource)

    def test_verify_requires_a_new_read_and_never_publishes_or_generates_audio(self):
        wordlist = str(uuid.uuid4())
        calls = []
        def request(method, path, body, token):
            calls.append((method, path, body))
            if path.endswith('/login'):
                return {'access_token': 'synthetic-token'}
            if method == 'POST':
                self.assertNotIn('audio_generation', body)
                return {'id': wordlist}
            if path.endswith('/items'):
                return {'items': [{'item_id': self.resource['item_id'], 'private_note': 'before'}]}
            if method == 'PUT':
                return {'revision': 2}
            revision = 2 if any(call[0] == 'PUT' for call in calls) else 1
            return {'state': 'draft', 'owner_user_id': self.resource['user_id'], 'revision': revision,
                    'name': 'Deployment smoke updated' if revision == 2 else 'Deployment smoke'}
        self.assertEqual(business.verify(request, self.resource), wordlist)
        put = next(index for index, call in enumerate(calls) if call[0] == 'PUT')
        self.assertTrue(any(call[0] == 'GET' and call[1].endswith(wordlist) for call in calls[put+1:]))
        self.assertFalse(any('/publish' in call[1] for call in calls))

    def test_schema_preflight_is_readonly_and_never_creates_a_fixture(self):
        sql = Mock(return_value='')
        business.check_schema(sql)
        statement = sql.call_args.args[0]
        self.assertTrue(statement.startswith('BEGIN READ ONLY;'))
        self.assertTrue(statement.endswith('ROLLBACK;'))
        for table in ('users', 'wordlist_audio_jobs', 'wordlist_update_requests', 'audit.admin_actions'):
            self.assertIn(' FROM ' + table + ' WHERE false;', statement)
        self.assertNotIn('INSERT ', statement)
        self.assertNotIn('DELETE ', statement)

    def test_auth_cleanup_uses_existing_schema_and_refuses_unknown_user_references(self):
        sql = Mock(side_effect=['', 'true'])
        business.cleanup_auth(sql, self.resource)
        statement = sql.call_args_list[0].args[0]
        self.assertIn('pg_constraint', statement)
        self.assertIn('information_schema.columns', statement)
        self.assertNotIn('FROM learning_review_items', statement)
        self.assertIn('BEGIN READ ONLY', sql.call_args_list[1].args[0])
        with self.assertRaisesRegex(SmokeFailure, 'auth_cleanup_residual'):
            business.cleanup_auth(Mock(side_effect=['', 'false']), self.resource)
        sql = Mock()
        business.check_auth_schema(sql)
        self.assertNotIn('wordlists', sql.call_args.args[0])

    def test_shared_ip_bucket_is_never_part_of_account_cleanup_key(self):
        key = business.account_throttle_key(self.resource['user_id'])
        self.assertTrue(key.startswith('auth:login:account:'))
        self.assertNotIn(self.resource['email'], key)
        self.assertNotIn(':ip:', key)


if __name__ == '__main__':
    unittest.main()
