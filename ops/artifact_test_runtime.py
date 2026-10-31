"""Task-owned Docker services for release tests. Never accepts an existing DB/Redis URL."""

import base64
import email.message
import hashlib
import http.cookiejar
import io
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


class VerificationError(Exception):
    pass


def execute(stage, args, *, data=None, timeout=120, environment=None):
    try:
        result = subprocess.run(args, input=data, env=environment, capture_output=True, timeout=timeout, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise VerificationError(stage) from error
    if result.returncode:
        raise VerificationError(stage)
    return result.stdout


def digest(path):
    result = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1048576), b""):
            result.update(block)
    return result.hexdigest()


def interrupted(signum, frame):
    raise VerificationError("interrupted")


def install_signals():
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)


def write_receipt(output, receipt):
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(output.name + ".partial")
    with temporary.open("w", encoding="utf-8") as target:
        json.dump(receipt, target, indent=2, sort_keys=True)
        target.write("\n")
        target.flush()
        os.fsync(target.fileno())
    temporary.replace(output)


class IsolatedRuntime:
    def __init__(self, *, postgres_image="postgres:18-alpine", redis_image="redis:6.0-alpine", app_image="ubuntu:22.04"):
        self.owner = uuid.uuid4().hex
        self.prefix = "tsz-artifact-" + self.owner
        self.images = {"postgresql": postgres_image, "redis": redis_image, "app": app_image, "probe": "python:3.12-alpine"}
        self.resources = []
        self.network_id = None
        self.app_id = None
        self.http_id = None
        self.pg_id = None
        self.redis_id = None
        self.cleanup = {"complete": False, "remaining": []}
        self.jwt = secrets.token_hex(32)
        self.admin_jwt = secrets.token_hex(32)
        self.environment = {}

    def docker(self, stage, *args, **kwargs):
        environment = dict(os.environ)
        args = list(args)
        for index, value in enumerate(args[:-1]):
            if value == "-e" and "=" in args[index + 1]:
                key, content = args[index + 1].split("=", 1)
                environment[key] = content
                args[index + 1] = key
        return execute(stage, ["docker", *args], environment=environment, **kwargs)

    def create(self, label, image, *args):
        identifier = self.docker("create_" + label, "create", "--label", "tsz.artifact-test=" + self.owner,
                                 "--network", self.network_id, *args, image).decode().strip()
        self.resources.append(identifier)
        return identifier

    def inspect(self, identifier):
        return json.loads(self.docker("inspect_resource", "inspect", identifier))[0]

    def verify_owner(self, identifier):
        if self.inspect(identifier).get("Config", {}).get("Labels", {}).get("tsz.artifact-test") != self.owner:
            raise VerificationError("resource_owner_mismatch")

    def start(self):
        for label, image in self.images.items():
            item = json.loads(self.docker("image_not_cached_" + label, "image", "inspect", image))[0]
            self.images[label] = item["Id"]
        self.network_id = self.docker("create_network", "network", "create", "--internal", "--label",
                                      "tsz.artifact-test=" + self.owner, self.prefix).decode().strip()
        self.pg_id = self.create("postgresql", self.images["postgresql"], "--name", self.prefix + "-pg",
                                 "--tmpfs", "/var/lib/postgresql", "-e", "POSTGRES_HOST_AUTH_METHOD=trust",
                                 "-e", "POSTGRES_DB=artifact_test", "-e", "PGDATA=/var/lib/postgresql/artifact")
        self.redis_id = self.create("redis", self.images["redis"], "--name", self.prefix + "-redis")
        self.http_id = self.docker("create_http_probe", "create", "--label", "tsz.artifact-test=" + self.owner,
                                   "--network", self.network_id, "--entrypoint", "sh", self.images["probe"],
                                   "-c", "while :; do sleep 3600; done").decode().strip()
        self.resources.append(self.http_id)
        for identifier in (self.pg_id, self.redis_id, self.http_id):
            self.docker("start_dependency", "start", identifier)
        for attempt in range(60):
            try:
                self.sql("SELECT 1")
                if self.docker("redis_ping", "exec", self.redis_id, "redis-cli", "ping").strip() != b"PONG":
                    raise VerificationError("redis_not_ready")
                break
            except VerificationError:
                if attempt == 59:
                    raise
                time.sleep(0.5)
        self.environment = {
            "DATABASE_URL": "postgresql://postgres@" + self.prefix + "-pg:5432/artifact_test",
            "REDIS_URL": "redis://" + self.prefix + "-redis:6379/0",
            "JWT_SECRET": self.jwt, "ADMIN_JWT_SECRET": self.admin_jwt,
            "BIND_IP": "0.0.0.0", "PORT": "8383", "COOKIE_SECURE": "false",
            "DEPLOYMENT_SMOKE_ONLY": "true", "INVITATION_REWARD_ENABLED": "false",
            "LOGIN_REWARD_ENABLED": "false", "LEARNING_MEMORY_ENABLED": "false",
        }
        return {"postgresql": self.sql("SHOW server_version"),
                "redis": self.docker("redis_version", "exec", self.redis_id, "redis-cli", "INFO", "server").decode().split("redis_version:")[1].splitlines()[0],
                "images": self.images, "network_internal": True}

    def sql(self, statement):
        return self.docker("isolated_sql", "exec", "-i", self.pg_id, "psql", "-X", "-qAt", "-v",
                           "ON_ERROR_STOP=1", "-U", "postgres", "-d", "artifact_test",
                           data=("SET statement_timeout='30s';\n" + statement).encode()).decode().strip()

    def ledger(self):
        return json.loads(self.sql("SELECT coalesce(json_agg(json_build_object('version',version,'checksum',encode(checksum,'hex'),'success',success) ORDER BY version),'[]') FROM _sqlx_migrations"))

    def binary(self, artifact, *arguments, timeout=120):
        identifier = self.docker("create_binary_command", "create", "--label", "tsz.artifact-test=" + self.owner,
                                 "--network", self.network_id, "--platform", "linux/amd64",
                                 "--mount", "type=bind,src=" + str(Path(artifact).resolve()) + ",dst=/artifact,readonly",
                                 "--entrypoint", "/artifact",
                                 *sum((["-e", key + "=" + value] for key, value in self.environment.items()), []),
                                 self.images["app"], *arguments).decode().strip()
        self.resources.append(identifier)
        self.docker("start_binary_command", "start", identifier)
        self.docker("wait_binary_command", "wait", identifier, timeout=timeout)
        state = self.inspect(identifier)["State"]
        if state["ExitCode"] != 0:
            raise VerificationError("binary_command_failed")
        return self.docker("binary_command_output", "logs", identifier).decode()

    def start_app(self, artifact):
        if self.app_id is not None:
            raise VerificationError("app_already_started")
        self.app_id = self.create("application", self.images["app"], "--platform", "linux/amd64",
                                  "--mount", "type=bind,src=" + str(Path(artifact).resolve()) + ",dst=/artifact,readonly",
                                  "--entrypoint", "/artifact",
                                  *sum((["-e", key + "=" + value] for key, value in self.environment.items()), []))
        self.docker("start_application", "start", self.app_id)
        state = self.inspect(self.app_id)
        if state["HostConfig"]["PortBindings"]:
            raise VerificationError("application_ports_must_not_be_published")
        self.app_host = next(iter(state["NetworkSettings"]["Networks"].values()))["IPAddress"]
        for attempt in range(120):
            if not self.inspect(self.app_id)["State"]["Running"]:
                raise VerificationError("application_exited")
            try:
                client = self.client()
                if client.request("GET", "/readyz", expected=200) != {"status": "ready"}:
                    raise VerificationError("invalid_readiness")
                if client.request("GET", "/healthz", expected=200) != {"status": "ok"}:
                    raise VerificationError("invalid_health")
                return
            except (VerificationError, OSError):
                if attempt == 119:
                    raise VerificationError("application_readiness_timeout")
                time.sleep(0.5)

    def stop_app(self):
        if self.app_id is not None:
            self.verify_owner(self.app_id)
            self.docker("stop_application", "stop", "--time", "10", self.app_id)
            self.app_id = None

    def client(self):
        return Client("http://" + self.app_host + ":8383", transport=self.http)

    def http(self, request):
        url = urllib.parse.urlsplit(request.full_url)
        payload = {"method": request.get_method(), "path": url.path + ("?" + url.query if url.query else ""),
                   "headers": dict(request.header_items()), "body": base64.b64encode(request.data or b"").decode()}
        probe = "import base64,http.client,json,sys; q=json.load(sys.stdin); c=http.client.HTTPConnection(sys.argv[1],8383,timeout=10); c.request(q['method'],q['path'],base64.b64decode(q['body']),q['headers']); r=c.getresponse(); print(json.dumps({'status':r.status,'headers':r.getheaders(),'body':base64.b64encode(r.read(1048577)).decode()})); c.close()"
        raw = self.docker("http_probe", "exec", "-i", self.http_id, "python3", "-I", "-c", probe, self.app_host,
                          data=json.dumps(payload).encode(), timeout=15)
        response = json.loads(raw)
        result = io.BytesIO(base64.b64decode(response["body"]))
        result.status = result.code = response["status"]
        result.headers = email.message.Message()
        for key, value in response["headers"]:
            result.headers[key] = value
        return result

    def seed_user(self):
        user = str(uuid.uuid4())
        # Fixed synthetic password/hash pair, never a real account credential.
        self.sql("INSERT INTO users(id,phone,email,password_hash,display_name) VALUES('" + user
                 + "','19900000001','artifact-smoke@example.test','$2y$10$ZaSuM4NVdb7gIG0GEPjq6uCummPWr2WWt4MM6IGY1D4huEAX95Qea','Artifact smoke');"
                 + "INSERT INTO user_roles(user_id,role) VALUES('" + user + "','student');")
        return user

    def table_columns(self):
        return json.loads(self.sql("SELECT coalesce(json_object_agg(table_schema || '.' || table_name, columns), '{}') FROM (SELECT table_schema, table_name, json_agg(column_name ORDER BY ordinal_position) AS columns FROM information_schema.columns WHERE table_schema NOT IN ('pg_catalog','information_schema') GROUP BY table_schema,table_name) schema_columns"))

    def snapshot(self, *, projection=None):
        tables = json.loads(self.sql("SELECT coalesce(json_agg(json_build_array(schemaname,tablename) ORDER BY schemaname,tablename),'[]') FROM pg_tables WHERE schemaname NOT IN ('pg_catalog','information_schema')"))
        result = {}
        schema_dump = self.docker("schema_snapshot", "exec", self.pg_id, "pg_dump", "-U", "postgres", "-d", "artifact_test", "--schema-only", "--no-owner", "--no-privileges").decode()
        schema_dump = "\n".join(line for line in schema_dump.splitlines() if not line.startswith(("\\restrict ", "\\unrestrict ")))
        result["schema_sha256"] = hashlib.sha256(schema_dump.encode()).hexdigest()
        sequences = json.loads(self.sql("SELECT coalesce(json_agg(row_to_json(s) ORDER BY schemaname,sequencename),'[]') FROM pg_sequences s WHERE schemaname NOT IN ('pg_catalog','information_schema')"))
        result["sequences"] = sequences
        for schema, table in tables:
            quoted = '"' + schema.replace('"', '""') + '"."' + table.replace('"', '""') + '"'
            row = "to_jsonb(t)"
            if projection is not None and schema + "." + table in projection:
                columns = ",".join("'" + name.replace("'", "''") + "'" for name in projection[schema + "." + table])
                row = "(SELECT coalesce(jsonb_object_agg(k,v),'{}'::jsonb) FROM jsonb_each(to_jsonb(t)) projected(k,v) WHERE k=ANY(ARRAY[" + columns + "]::text[]))"
            result[schema + "." + table] = json.loads(self.sql("SELECT json_build_object('rows',count(*),'digest',md5(coalesce(string_agg(md5(" + row + "::text),'' ORDER BY md5(" + row + "::text)),''))) FROM " + quoted + " t"))
        return result

    def close(self):
        failures = []
        for identifier in reversed(self.resources):
            try:
                self.verify_owner(identifier)
                self.docker("cleanup_container", "rm", "--force", "--volumes", identifier)
                remaining = self.docker("verify_container_removed", "container", "ls", "--all", "--quiet", "--no-trunc", "--filter", "id=" + identifier)
                if remaining.strip():
                    raise VerificationError("container_still_exists")
            except (VerificationError, subprocess.TimeoutExpired):
                failures.append(identifier)
        if self.network_id is not None and not failures:
            try:
                item = json.loads(self.docker("inspect_network", "network", "inspect", self.network_id))[0]
                if item.get("Labels", {}).get("tsz.artifact-test") != self.owner:
                    raise VerificationError("network_owner_mismatch")
                self.docker("cleanup_network", "network", "rm", self.network_id)
            except VerificationError:
                failures.append(self.network_id)
        if not failures:
            try:
                containers = self.docker("audit_owned_containers", "container", "ls", "--all", "--quiet", "--filter", "label=tsz.artifact-test=" + self.owner)
                networks = self.docker("audit_owned_networks", "network", "ls", "--quiet", "--filter", "label=tsz.artifact-test=" + self.owner)
                failures.extend(containers.decode().split())
                failures.extend(networks.decode().split())
            except VerificationError:
                failures.append("cleanup_audit_unavailable:" + self.owner)
        if self.network_id is not None and failures and self.network_id not in failures:
            failures.append(self.network_id)
        self.cleanup = {"complete": not failures, "remaining": failures}
        if failures:
            raise VerificationError("cleanup_failed")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Client:
    def __init__(self, base, transport=None):
        self.base = base
        self.transport = transport
        self.cookies = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect(),
                                                  urllib.request.HTTPCookieProcessor(self.cookies))
        self.token = None

    def request(self, method, path, body=None, *, expected=200, authenticated=False):
        headers = {"Content-Type": "application/json", "X-TSZ-Wordlist-Custom-Items": "v1", "X-TSZ-Wordlist-Annotations": "v1"}
        if authenticated:
            headers["Authorization"] = "Bearer " + self.token
        request = urllib.request.Request(self.base + path, method=method, headers=headers,
                                         data=json.dumps(body).encode() if body is not None else None)
        if self.transport is not None:
            self.cookies.add_cookie_header(request)
            response = self.transport(request)
            response.info = lambda: response.headers
            self.cookies.extract_cookies(response, request)
        else:
            try:
                response = self.opener.open(request, timeout=10)
            except urllib.error.HTTPError as error:
                response = error
        with response:
            data = response.read(1048576)
            if response.status != expected:
                raise VerificationError("unexpected_http_status_" + str(response.status))
            return json.loads(data) if data else None

    def login(self):
        value = self.request("POST", "/api/v1/auth/login", {"identifier": "artifact-smoke@example.test", "password": "ArtifactSmoke-Only-2026!"})
        self.token = value["access_token"]
        before = next((cookie.value for cookie in self.cookies if cookie.name == "refresh_token"), None)
        if not before:
            raise VerificationError("refresh_cookie_missing")
        value = self.request("POST", "/api/v1/auth/refresh")
        self.token = value["access_token"]
        if next((cookie.value for cookie in self.cookies if cookie.name == "refresh_token"), None) in (None, before):
            raise VerificationError("refresh_not_rotated")

    def roundtrip(self, *, wordlist=None):
        self.login()
        if wordlist is None:
            item = str(uuid.uuid4())
            value = self.request("POST", "/api/v1/me/wordlists", {
                "idempotency_key": str(uuid.uuid4()), "name": "artifact-smoke",
                "items": [{"item_id": item, "custom_word": {"word": "smoke", "definition": "synthetic fixture"}, "private_note": "before"}],
            }, authenticated=True)
            wordlist = {"id": value["id"], "item_id": item}
        uuid.UUID(wordlist["id"])
        uuid.UUID(wordlist["item_id"])
        path = "/api/v1/me/wordlists/" + wordlist["id"]
        original = self.request("GET", path, authenticated=True)
        self.request("GET", path, expected=401)
        self.request("PUT", path, {"expected_revision": original["revision"],
                                   "content": {"name": "artifact-smoke-updated", "items": [{"item_id": wordlist["item_id"], "custom_word": {"word": "smoke", "definition": "synthetic fixture"}}]},
                                   "note_updates": []}, authenticated=True)
        stored = self.request("GET", path, authenticated=True)
        items = self.request("GET", path + "/items", authenticated=True)
        expected_item = next((item for item in items["items"] if item.get("item_id") == wordlist["item_id"]), None)
        if (stored["name"] != "artifact-smoke-updated"
                or stored["revision"] != original["revision"] + 1
                or expected_item is None
                or expected_item.get("custom_word") != {"word": "smoke", "definition": "synthetic fixture"}
                or expected_item.get("private_note") != "before"):
            raise VerificationError("wordlist_persistence_failed")
        self.request("POST", "/api/v1/auth/logout", expected=204)
        if any(cookie.name == "refresh_token" for cookie in self.cookies):
            raise VerificationError("logout_cookie_not_cleared")
        return wordlist
