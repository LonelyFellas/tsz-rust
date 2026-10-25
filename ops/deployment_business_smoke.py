"""One owned draft/custom-item fixture; never publishes, buys audio or grants rewards."""

import base64
import hashlib
import json
import re
import secrets
import uuid

from deployment_auth_smoke import SmokeFailure, require


PASSWORD = "ArtifactSmoke-Only-2026!"
PASSWORD_HASH = "$2y$10$ZaSuM4NVdb7gIG0GEPjq6uCummPWr2WWt4MM6IGY1D4huEAX95Qea"


def resources(owner):
    namespace = uuid.UUID(owner)
    user = str(uuid.uuid5(namespace, "business-user"))
    return {"owner": owner, "user_id": user, "item_id": str(uuid.uuid5(namespace, "business-item")),
            "create_key": str(uuid.uuid5(namespace, "business-create")),
            "email": "deploy-smoke-" + namespace.hex + "@example.invalid",
            "phone": "199" + str(namespace.int % 100000000).zfill(8),
            "credential_fingerprint": hashlib.md5(PASSWORD_HASH.encode()).hexdigest()}


def random_credentials():
    # Ubuntu's Python 3.10 provides libcrypt bcrypt. Fail preflight if unavailable;
    # do not install packages or fall back to a public fixed smoke credential.
    try:
        import crypt
        password = secrets.token_urlsafe(32)
        hashed = crypt.crypt(password, crypt.mksalt(crypt.METHOD_BLOWFISH, rounds=1024))
    except (ImportError, AttributeError, ValueError, OSError):
        raise SmokeFailure('bcrypt_generator_unavailable') from None
    require(isinstance(hashed, str) and hashed.startswith(('$2a$', '$2b$', '$2y$')), 'bcrypt_generator_unavailable')
    return password, hashed


def validate_resource(resource):
    expected = resources(resource["owner"])
    expected['credential_fingerprint'] = resource.get('credential_fingerprint')
    require(resource == expected and re.fullmatch(r'[0-9a-f]{32}', resource.get('credential_fingerprint', '')), 'invalid_fixture_identity')


def assert_absent(sql, resource):
    validate_resource(resource)
    r = resource
    existing = json.loads(sql("SELECT coalesce(json_agg(id),'[]') FROM users WHERE id='" + r["user_id"]
                              + "' OR email='" + r["email"] + "' OR phone='" + r["phone"] + "'"))
    require(existing == [], "business_fixture_identity_exists")


def check_auth_schema(sql):
    sql('BEGIN READ ONLY; SELECT id,phone,email,password_hash,display_name FROM users WHERE false;'
        'SELECT user_id,role FROM user_roles WHERE false; SELECT user_id FROM refresh_tokens WHERE false; ROLLBACK;')


def cleanup_auth(sql, resource):
    """Auth creates only user/role/token rows; refuse every other incoming user reference."""
    validate_resource(resource)
    u = resource['user_id']
    sql(f"""
    BEGIN;
    SET LOCAL lock_timeout='5s';
    SELECT id FROM users WHERE id='{u}' FOR UPDATE;
    DO $$ DECLARE ref record; present boolean; BEGIN
      IF EXISTS(SELECT 1 FROM users WHERE id='{u}' AND (email IS DISTINCT FROM '{resource['email']}'
        OR phone IS DISTINCT FROM '{resource['phone']}' OR md5(password_hash) IS DISTINCT FROM '{resource['credential_fingerprint']}'
        OR display_name IS DISTINCT FROM 'Deployment smoke'))
        OR EXISTS(SELECT 1 FROM user_roles WHERE user_id='{u}' AND role<>'student')
      THEN RAISE EXCEPTION 'auth fixture ownership guard'; END IF;
      FOR ref IN
        SELECT ns.nspname AS schema_name,c.relname AS table_name,a.attname AS column_name
        FROM pg_constraint fk JOIN pg_class c ON c.oid=fk.conrelid
        JOIN pg_namespace ns ON ns.oid=c.relnamespace
        JOIN pg_attribute a ON a.attrelid=c.oid AND a.attnum=ANY(fk.conkey)
        WHERE fk.contype='f' AND fk.confrelid='users'::regclass
          AND fk.conrelid NOT IN ('user_roles'::regclass,'refresh_tokens'::regclass)
        UNION
        SELECT table_schema,table_name,column_name FROM information_schema.columns
        WHERE (table_schema,table_name,column_name) IN
          (('public','coin_wallets','owner_id'),('public','coin_operations','actor_id'),
           ('public','wordlist_tips','payer_user_id'),('public','wordlist_tips','author_user_id'),
           ('public','wordlist_audio_jobs','user_id'),('public','wordlist_deletion_events','deleted_by_user_id'),
           ('platform','outbox_events','aggregate_id'),('audit','admin_actions','resource_id'))
      LOOP
        EXECUTE format('SELECT EXISTS(SELECT 1 FROM %I.%I WHERE %I::text=$1)',ref.schema_name,ref.table_name,ref.column_name)
          INTO present USING '{u}';
        IF present THEN RAISE EXCEPTION 'auth fixture external reference guard'; END IF;
      END LOOP;
    END $$;
    DELETE FROM users WHERE id='{u}' AND email='{resource['email']}' AND phone='{resource['phone']}';
    COMMIT;
    """)
    absent = sql(f"BEGIN READ ONLY; SELECT (NOT EXISTS(SELECT 1 FROM users WHERE id='{u}') AND NOT EXISTS(SELECT 1 FROM user_roles WHERE user_id='{u}') AND NOT EXISTS(SELECT 1 FROM refresh_tokens WHERE user_id='{u}'))::text; ROLLBACK;")
    require(absent.splitlines()[-1] == 'true', 'auth_cleanup_residual')


def check_schema(sql):
    # Parse every relation/column needed by cleanup before any user is created.
    columns = {
        'users': 'id,phone,email,password_hash,display_name',
        'user_roles': 'user_id,role', 'refresh_tokens': 'user_id',
        'wordlists': 'id,owner_user_id,owner_admin_id,create_key,state',
        'wordlist_items': 'wordlist_id,item_id,entry_id',
        'wordlist_review_requests': 'wordlist_id',
        'wordlist_audio_jobs': 'user_id,wordlist_id',
        'coin_wallets': 'owner_type,owner_id', 'coin_operations': 'actor_id',
        'wordlist_tips': 'payer_user_id,author_user_id,wordlist_id',
        'account_deletion_requests': 'user_id', 'student_profiles': 'user_id',
        'teacher_profiles': 'user_id', 'learning_memories': 'user_id',
        'learning_memory_enrollments': 'user_id',
        'user_notifications': 'user_id,actor_user_id,wordlist_id',
        'wordlist_library_nodes': 'user_id,wordlist_id',
        'wordlist_interactions': 'user_id,wordlist_id',
        'wordlist_deletion_events': 'deleted_by_user_id,wordlist_id',
        'learning_tasks': 'user_id,wordlist_ids',
        'learning_questions': 'source_wordlist_id',
        'learning_cycle_units': 'source_wordlist_id',
        'learning_memory_sources': 'user_id,wordlist_id',
        'learning_review_items': 'source_wordlist_id',
        'wordlist_update_requests': 'user_id',
        'platform.outbox_events': 'aggregate_id', 'audit.admin_actions': 'resource_id',
    }
    sql('BEGIN READ ONLY;' + ''.join('SELECT ' + names + ' FROM ' + table + ' WHERE false;'
                                    for table, names in columns.items()) + 'ROLLBACK;')


def seed(sql, resource, *, credential_hash=PASSWORD_HASH):
    # The host records assert_absent before calling this transaction. On an ambiguous
    # response, recovery only cleans a fixture whose absence was observed beforehand.
    assert_absent(sql, resource)
    r = resource
    require(re.fullmatch(r'\$2[aby]\$[0-9]{2}\$[./A-Za-z0-9]{53}', credential_hash)
            and hashlib.md5(credential_hash.encode()).hexdigest() == r['credential_fingerprint'], 'invalid_fixture_credential')
    sql("BEGIN; INSERT INTO users(id,phone,email,password_hash,display_name) VALUES('" + r["user_id"]
        + "','" + r["phone"] + "','" + r["email"] + "','" + credential_hash + "','Deployment smoke');"
        + "INSERT INTO user_roles(user_id,role) VALUES('" + r["user_id"] + "','student'); COMMIT;")


def verify(request, resource, *, password=PASSWORD):
    """request(method,path,body,token) uses the same TLS/CookieJar as auth smoke."""
    r = resource
    login = request("POST", "/api/v1/auth/login", {"identifier": r["email"], "password": password}, None)
    token = login["access_token"]
    created = request("POST", "/api/v1/me/wordlists", {
        "idempotency_key": r["create_key"], "name": "Deployment smoke",
        "items": [{"item_id": r["item_id"], "custom_word": {"word": "smoke", "definition": "synthetic fixture"}, "private_note": "before"}],
    }, token)
    identifier = str(uuid.UUID(created["id"]))
    path = "/api/v1/me/wordlists/" + identifier
    old = request("GET", path, None, token)
    require(old["state"] == "draft" and old["owner_user_id"] == r["user_id"], "business_owner_mismatch")
    request("PUT", path, {"expected_revision": old["revision"],
                         "content": {"name": "Deployment smoke updated", "items": [{"item_id": r["item_id"], "custom_word": {"word": "smoke", "definition": "synthetic fixture"}}]},
                         "note_updates": []}, token)
    stored = request("GET", path, None, token)
    items = request("GET", path + "/items", None, token)["items"]
    require(stored["name"] == "Deployment smoke updated" and stored["revision"] == old["revision"] + 1
            and len(items) == 1 and items[0]["item_id"] == r["item_id"]
            and items[0]["private_note"] == "before", "business_persistence_failed")
    return identifier


def cleanup(sql, resource, *, known_ids=(), record_ids=None):
    """Lock the dedicated user first. A committed/ambiguous API create is found by exact create_key."""
    validate_resource(resource)
    u, k, i = resource["user_id"], resource["create_key"], resource["item_id"]
    identifiers = sorted(set(known_ids) | set(json.loads(sql(f"SELECT coalesce(json_agg(id),'[]') FROM wordlists WHERE owner_user_id='{u}'"))))
    require(len(identifiers) <= 1, "unexpected_fixture_wordlists")
    for identifier in identifiers:
        require(str(uuid.UUID(identifier)) == identifier, "invalid_fixture_wordlist_id")
    if record_ids is not None:
        record_ids(identifiers)
    wordlists = "ARRAY[" + ",".join("'" + str(uuid.UUID(value)) + "'::uuid" for value in identifiers) + "]::uuid[]"
    # Never delete other users' notifications, financial facts or work already claimed by a worker.
    guards = f"""
      EXISTS(SELECT 1 FROM wordlist_audio_jobs WHERE user_id='{u}' OR wordlist_id =ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM coin_wallets WHERE owner_type='user' AND owner_id='{u}')
      OR EXISTS(SELECT 1 FROM coin_operations WHERE actor_id='{u}')
      OR EXISTS(SELECT 1 FROM wordlist_tips WHERE payer_user_id='{u}' OR author_user_id='{u}' OR wordlist_id =ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM account_deletion_requests WHERE user_id='{u}')
      OR EXISTS(SELECT 1 FROM student_profiles WHERE user_id='{u}')
      OR EXISTS(SELECT 1 FROM teacher_profiles WHERE user_id='{u}')
      OR EXISTS(SELECT 1 FROM learning_memories WHERE user_id='{u}')
      OR EXISTS(SELECT 1 FROM learning_memory_enrollments WHERE user_id='{u}')
      OR EXISTS(SELECT 1 FROM user_notifications WHERE user_id='{u}')
      OR EXISTS(SELECT 1 FROM user_roles WHERE user_id='{u}' AND role<>'student')
      OR EXISTS(SELECT 1 FROM wordlist_library_nodes WHERE user_id='{u}' AND (space<>'owned' OR wordlist_id IS NULL OR wordlist_id<>ALL({wordlists})))
      OR EXISTS(SELECT 1 FROM wordlist_review_requests WHERE wordlist_id=ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM user_notifications WHERE user_id<>'{u}' AND (actor_user_id='{u}' OR wordlist_id =ANY({wordlists})))
      OR EXISTS(SELECT 1 FROM wordlist_library_nodes WHERE user_id<>'{u}' AND wordlist_id =ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM wordlist_interactions WHERE user_id<>'{u}' AND wordlist_id =ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM wordlist_deletion_events WHERE deleted_by_user_id='{u}' OR wordlist_id =ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM learning_tasks WHERE user_id='{u}' OR wordlist_ids && {wordlists})
      OR EXISTS(SELECT 1 FROM learning_questions WHERE source_wordlist_id =ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM learning_cycle_units WHERE source_wordlist_id =ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM learning_memory_sources WHERE user_id='{u}' OR wordlist_id =ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM learning_review_items WHERE source_wordlist_id =ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM platform.outbox_events WHERE aggregate_id IN ('{u}'::uuid,'{i}'::uuid) OR aggregate_id =ANY({wordlists}))
      OR EXISTS(SELECT 1 FROM audit.admin_actions WHERE resource_id IN ('{u}'::uuid,'{i}'::uuid) OR resource_id =ANY({wordlists}))
    """
    sql(f"""
    BEGIN;
    SET LOCAL lock_timeout='5s';
    SELECT id FROM users WHERE id='{u}' FOR UPDATE;
    SELECT pg_advisory_xact_lock(hashtextextended('wordlist-library:'||'{u}',0));
    SELECT id FROM wordlists WHERE owner_user_id='{u}' FOR UPDATE;
    DO $$ BEGIN
      IF EXISTS(SELECT 1 FROM users WHERE id='{u}' AND (email IS DISTINCT FROM '{resource['email']}' OR phone IS DISTINCT FROM '{resource['phone']}' OR md5(password_hash) IS DISTINCT FROM '{resource['credential_fingerprint']}' OR display_name IS DISTINCT FROM 'Deployment smoke'))
        OR EXISTS(SELECT 1 FROM wordlists WHERE owner_user_id='{u}' AND (id<>ALL({wordlists}) OR create_key<>'{k}' OR state<>'draft' OR owner_admin_id IS NOT NULL))
        OR EXISTS(SELECT 1 FROM wordlist_items WHERE wordlist_id =ANY({wordlists}) AND (item_id<>'{i}' OR entry_id IS NOT NULL))
        OR ({guards}) THEN RAISE EXCEPTION 'deployment fixture ownership or external reference guard'; END IF;
    END $$;
    DELETE FROM wordlists WHERE owner_user_id='{u}' AND create_key='{k}' AND state='draft';
    DELETE FROM users WHERE id='{u}' AND email='{resource['email']}' AND phone='{resource['phone']}';
    COMMIT;
    """)
    absent = sql(f"BEGIN READ ONLY; SELECT (NOT EXISTS(SELECT 1 FROM users WHERE id='{u}') AND NOT EXISTS(SELECT 1 FROM wordlists WHERE owner_user_id='{u}' OR create_key='{k}') AND NOT EXISTS(SELECT 1 FROM refresh_tokens WHERE user_id='{u}') AND NOT EXISTS(SELECT 1 FROM user_roles WHERE user_id='{u}') AND NOT EXISTS(SELECT 1 FROM wordlist_items WHERE item_id='{i}') AND NOT EXISTS(SELECT 1 FROM wordlist_library_nodes WHERE user_id='{u}') AND NOT EXISTS(SELECT 1 FROM wordlist_update_requests WHERE user_id='{u}') AND NOT EXISTS(SELECT 1 FROM student_profiles WHERE user_id='{u}') AND NOT EXISTS(SELECT 1 FROM teacher_profiles WHERE user_id='{u}') AND NOT EXISTS(SELECT 1 FROM user_notifications WHERE user_id='{u}') AND NOT ({guards}))::text; ROLLBACK;")
    require(absent.splitlines()[-1] == "true", "business_cleanup_residual")


def account_throttle_key(user):
    encoded = base64.urlsafe_b64encode(hashlib.sha256(("user:" + user).encode()).digest()).rstrip(b"=").decode()
    return "auth:login:account:" + encoded
