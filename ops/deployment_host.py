"""Fixed server-side operations, streamed from a trusted checkout by deployment_transport."""

import contextlib
import fcntl
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid

import deployment_auth_smoke as auth
import deployment_business_smoke as business
import deployment_manifest as manifests
from postgresql_backup_job import inspect_status as inspect_backup_status


ROOT = Path("/opt/tsz-rust")
LOCK = ROOT / "deploy.lock"
BINARY = ROOT / "target/release/tsz-rust"
MANIFEST = Path("/opt/tsz-deploy-manifests/api.json")
TRANSACTIONS = ROOT / "deployment-transactions"
OPERATION_FDS = []


class HostError(Exception):
    pass


def require(condition, stage):
    if not condition:
        raise HostError(stage)


def run(arguments, *, data=None, environment=None, timeout=30):
    try:
        result = subprocess.run(arguments, input=data, env=environment, capture_output=True,
                                timeout=timeout, check=False, pass_fds=tuple(OPERATION_FDS))
    except (OSError, subprocess.TimeoutExpired) as error:
        raise HostError("host_command_unavailable") from error
    require(result.returncode == 0, "host_command_failed")
    return result.stdout


def sha(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1048576), b""):
            value.update(block)
    return value.hexdigest()


def atomic(path, value):
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as target:
            temporary = Path(target.name)
            os.chmod(temporary, 0o600)
            json.dump(value, target)
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, path)
        temporary = None
        descriptor = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def transaction(plan):
    owner = plan["deployment_id"]
    require(re.fullmatch(r"[0-9a-f-]{36}", owner), "invalid_owner")
    return TRANSACTIONS / owner


def owner_check(plan):
    require(LOCK.is_dir() and (LOCK / "owner").read_text().strip() == plan["deployment_id"], "owner_mismatch")


@contextlib.contextmanager
def operation(plan):
    owner_check(plan)
    # All mutating commands and observation serialize against the same inode. This lock
    # lives in the retained transaction directory, not in the removable owner directory.
    with (transaction(plan) / "operation.lock").open("r+") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise HostError("remote_operation_busy") from None
        owner_check(plan)
        OPERATION_FDS.append(lock.fileno())
        try:
            yield
        finally:
            OPERATION_FDS.remove(lock.fileno())


def state_path(plan):
    return transaction(plan) / "state.json"


def state(plan):
    value = json.loads(state_path(plan).read_text())
    plan_digest = hashlib.sha256(json.dumps(plan, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    require(value["owner"] == plan["deployment_id"] and value["sha"] == plan["source"]["git_sha"]
            and value["plan_sha256"] == plan_digest, "remote_state_mismatch")
    return value


def configuration():
    # Read, never execute the environment file. Reject expansion-dependent forms.
    values = {}
    for line in (ROOT / ".env").read_text().splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if line.startswith("export "):
            line = line[7:]
        key, separator, value = line.partition("=")
        require(separator and re.fullmatch(r"[A-Z][A-Z0-9_]*", key.strip()), "unsupported_environment_file")
        tokens = shlex.split(value, comments=True, posix=True)
        require(len(tokens) <= 1 and "$" not in value and "`" not in value, "unsupported_environment_expansion")
        values[key.strip()] = tokens[0] if tokens else ""
    require(values.get("COOKIE_SECURE", "true") == "true", "cookie_secure_required")
    return values


def sql(statement, *, readonly=False):
    import urllib.parse
    cfg = configuration()
    url = urllib.parse.urlsplit(cfg["DATABASE_URL"])
    require(url.scheme in {"postgres", "postgresql"} and url.hostname, "invalid_database_configuration")
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LC_ALL": "C",
           "PGHOST": url.hostname, "PGPORT": str(url.port or 5432), "PGDATABASE": urllib.parse.unquote(url.path.lstrip("/")),
           "PGUSER": urllib.parse.unquote(url.username or ""), "PGPASSWORD": urllib.parse.unquote(url.password or ""),
           "PGCONNECT_TIMEOUT": "8", "PGOPTIONS": "-c statement_timeout=10000 -c lock_timeout=5000"}
    if readonly:
        env["PGOPTIONS"] += " -c default_transaction_read_only=on"
    options = dict(urllib.parse.parse_qsl(url.query))
    for key, variable in {"sslmode": "PGSSLMODE", "sslrootcert": "PGSSLROOTCERT", "sslcert": "PGSSLCERT",
                          "sslkey": "PGSSLKEY", "channel_binding": "PGCHANNELBINDING"}.items():
        if key in options:
            env[variable] = options[key]
    return run(["psql", "-X", "-qAt", "-v", "ON_ERROR_STOP=1"], data=statement.encode(), environment=env).decode().strip()


def ledger():
    return json.loads(sql("SELECT json_agg(json_build_object('version',version,'checksum',encode(checksum,'hex'),'success',success) ORDER BY version) FROM _sqlx_migrations", readonly=True))


CONFIG_DEFAULTS = {"COOKIE_SECURE": "true", "LOGIN_REWARD_ENABLED": "false"}
CONFIG_KEYS = ("DATABASE_URL", "REDIS_URL", "JWT_SECRET", "ADMIN_JWT_SECRET", *CONFIG_DEFAULTS)


def configuration_identity(cfg):
    selected = {key: cfg.get(key, CONFIG_DEFAULTS.get(key, "")) for key in CONFIG_KEYS}
    return hashlib.sha256(json.dumps(selected, sort_keys=True).encode()).hexdigest()


def check_service_configuration(plan=None):
    cfg = configuration()
    settings = dict(line.split('=', 1) for line in run([
        'systemctl', 'show', 'tsz-rust', '-p', 'EnvironmentFiles', '-p', 'Environment', '-p', 'WorkingDirectory'
    ]).decode().splitlines() if '=' in line)
    require(settings.get('EnvironmentFiles') == str(ROOT / '.env') + ' (ignore_errors=no)'
            and settings.get('WorkingDirectory') == str(ROOT), 'unsupported_service_configuration')
    overrides = dict(item.split('=', 1) for item in shlex.split(settings.get('Environment', '')) if '=' in item)
    require(all(key not in overrides or overrides[key] == cfg.get(key, CONFIG_DEFAULTS.get(key, ''))
                for key in CONFIG_KEYS), 'service_configuration_override')
    if plan is not None:
        require(configuration_identity(cfg) == state(plan)['configuration_sha256'], 'deployment_configuration_changed')
    return cfg


def start_formal(plan):
    check_service_configuration(plan)
    run(['systemctl', 'start', 'tsz-rust'])


def check_running_configuration(plan=None):
    cfg = check_service_configuration(plan if plan is not None and state_path(plan).exists() else None)
    pid = int(run(["systemctl", "show", "tsz-rust", "-p", "MainPID", "--value"]).strip())
    require(pid > 0, "formal_pid_missing")
    running = dict(item.decode().split("=", 1) for item in Path(f"/proc/{pid}/environ").read_bytes().split(b"\0") if b"=" in item)
    configured = configuration_identity(cfg)
    require(configuration_identity(running) == configured, "running_configuration_mismatch")
    if plan is not None and state_path(plan).exists():
        require(state(plan)["configuration_sha256"] == configured, "deployment_configuration_changed")
    return configured


def baseline_matches(plan):
    check_running_configuration(plan)
    expected = plan["baseline"]
    actual = json.loads(MANIFEST.read_text())
    require(actual == expected["deployment_manifest"], "baseline_manifest_changed")
    require(sha(BINARY) == expected["service"]["file_sha256"], "baseline_binary_changed")
    pid = int(run(["systemctl", "show", "tsz-rust", "-p", "MainPID", "--value"]).strip())
    require(pid > 0 and sha(Path(f"/proc/{pid}/exe")) == expected["service"]["process_sha256"], "baseline_process_changed")
    require(ledger() == expected["postgresql"]["migration_ledger"], "baseline_ledger_changed")


def preflight(plan):
    require(not LOCK.exists(), "remote_deployment_busy")
    baseline_matches(plan)
    business.check_auth_schema(lambda statement: sql(statement, readonly=True))
    business.random_credentials()
    auth.preflight()
    return {"ready": True, "scheduled_backup": inspect_backup_status(ROOT / "backups/scheduled")}


def backup(plan):
    folder = transaction(plan) / "backup"
    if folder.exists():
        manifests.validate_backup(folder)
        return
    folder.mkdir(mode=0o700)
    shutil.copy2(BINARY, folder / "tsz-rust")
    shutil.copy2(MANIFEST, folder / "api.json")
    manifests.validate_backup(folder)
    for path in (folder / "tsz-rust", folder / "api.json", folder, folder.parent):
        descriptor = os.open(path, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    value = state(plan)
    value["backup_ready"] = True
    atomic(state_path(plan), value)


def stop_candidate(plan):
    value = state(plan)
    marker = ("TSZ_DEPLOYMENT_OWNER=" + plan["deployment_id"]).encode()
    found = []
    for process in Path("/proc").iterdir():
        if not process.name.isdigit():
            continue
        try:
            if marker in (process / "environ").read_bytes().split(b"\0"):
                found.append(int(process.name))
        except (FileNotFoundError, ProcessLookupError):
            continue
    require(len(found) <= 1, "multiple_candidate_processes")
    if found:
        require(not value.get("candidate_pid") or value["candidate_pid"] == found[0], "candidate_pid_mismatch")
        value["candidate_pid"] = found[0]
    if value.get("candidate_pid"):
        pid = value["candidate_pid"]
        process = Path(f"/proc/{pid}")
        if process.exists() and (process / "stat").read_text().split(")", 1)[1].strip().split()[0] != "Z":
            require(sha(process / "exe") == plan["artifact_sha256"], "candidate_pid_reused")
            environment = (process / "environ").read_bytes().split(b"\0")
            require(("TSZ_DEPLOYMENT_OWNER=" + plan["deployment_id"]).encode() in environment, "candidate_owner_mismatch")
            os.kill(pid, signal.SIGTERM)
            for attempt in range(100):
                if not process.exists():
                    break
                status = (process / "stat").read_text().split(")", 1)[1].strip().split()[0]
                if status == "Z":
                    break
                time.sleep(0.1)
            else:
                raise HostError("candidate_did_not_stop")
        value["candidate_pid"] = None
        atomic(state_path(plan), value)
        (LOCK / "candidate_pid").unlink(missing_ok=True)


def process_environment(plan):
    cfg = configuration()
    require(configuration_identity(cfg) == state(plan)["configuration_sha256"], "deployment_configuration_changed")
    return {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), **cfg}


def start_candidate(plan):
    require(int(run(["systemctl", "show", "tsz-rust", "-p", "MainPID", "--value"]).strip()) == 0, "formal_service_not_stopped")
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 18383))
    env = process_environment(plan)
    env.update(BIND_IP="127.0.0.1", PORT="18383", DEPLOYMENT_SMOKE_ONLY="true", TSZ_DEPLOYMENT_OWNER=plan["deployment_id"])
    log = transaction(plan) / "candidate.log"
    with log.open("ab") as output:
        process = subprocess.Popen([str(BINARY)], cwd=ROOT, env=env, stdin=subprocess.DEVNULL,
                                   stdout=output, stderr=output, start_new_session=True)
    value = state(plan)
    value["candidate_pid"] = process.pid
    atomic(state_path(plan), value)
    (LOCK / "candidate_pid").write_text(str(process.pid))
    for attempt in range(120):
        require(process.poll() is None, "candidate_start_failed")
        try:
            import http.client
            with contextlib.closing(http.client.HTTPConnection("127.0.0.1", 18383, timeout=2)) as client:
                client.request("GET", "/readyz")
                response = client.getresponse()
                if response.status == 200 and json.loads(response.read(4096)) == {"status": "ready"}:
                    return
        except (OSError, ValueError):
            pass
        time.sleep(0.25)
    raise HostError("candidate_readiness_timeout")


def wait_formal(expected):
    for attempt in range(120):
        try:
            auth.check_formal(expected)
            import http.client
            with contextlib.closing(http.client.HTTPConnection("127.0.0.1", 8383, timeout=2)) as client:
                client.request("GET", "/readyz")
                response = client.getresponse()
                if response.status == 200 and json.loads(response.read(4096)) == {"status": "ready"}:
                    return
        except (OSError, ValueError, auth.SmokeFailure):
            pass
        time.sleep(0.25)
    raise HostError("formal_readiness_timeout")


def audit_fixture_redis(resource):
    # Only inspect the exact account bucket. The IP bucket is shared normal service state.
    import urllib.parse
    uri = urllib.parse.urlsplit(configuration()["REDIS_URL"])
    require(uri.scheme in {"redis", "rediss"} and uri.hostname, "invalid_redis_configuration")
    connection = socket.create_connection((uri.hostname, uri.port or 6379), timeout=5)
    if uri.scheme == "rediss":
        connection = ssl.create_default_context().wrap_socket(connection, server_hostname=uri.hostname)
    connection.settimeout(5)
    stream = connection.makefile("rb")
    def request(*parts):
        encoded = [str(value).encode() for value in parts]
        raw = b"*" + str(len(encoded)).encode() + b"\r\n"
        for value in encoded:
            raw += b"$" + str(len(value)).encode() + b"\r\n" + value + b"\r\n"
        connection.sendall(raw)
        kind, line = stream.read(1), stream.readline().rstrip(b"\r\n")
        require(kind in {b"+", b":"}, "redis_audit_failed")
        return line.decode()
    try:
        if uri.password:
            parts = [urllib.parse.unquote(uri.password)]
            if uri.username:
                parts.insert(0, urllib.parse.unquote(uri.username))
            require(request("AUTH", *parts) == "OK", "redis_audit_auth_failed")
        if uri.path.strip("/"):
            require(request("SELECT", int(uri.path.strip("/"))) == "OK", "redis_audit_db_failed")
        require(request("EXISTS", business.account_throttle_key(resource["user_id"])) == "0", "fixture_account_throttle_remains")
    finally:
        stream.close()
        connection.close()


def clean_fixture(plan, section, mode=None):
    value = state(plan)
    require(configuration_identity(configuration()) == value["configuration_sha256"], "deployment_configuration_changed")
    fixture = value[section][mode] if mode is not None else value[section]
    require(fixture.get("absent_before") is True, "fixture_ownership_not_proven")
    def record(identifiers):
        current = state(plan)
        target = current[section][mode] if mode is not None else current[section]
        target["wordlist_ids"] = identifiers
        atomic(state_path(plan), current)
    if section == "auth_fixtures":
        business.cleanup_auth(sql, fixture["resource"])
    else:
        business.cleanup(sql, fixture["resource"], known_ids=fixture.get("wordlist_ids", []), record_ids=record)
    audit_fixture_redis(fixture["resource"])
    current = state(plan)
    target = current[section][mode] if mode is not None else current[section]
    target["cleanup_complete"] = True
    atomic(state_path(plan), current)


def auth_smoke(plan, mode, credentials=None):
    require(configuration_identity(configuration()) == state(plan)["configuration_sha256"], "deployment_configuration_changed")
    if mode != "candidate":
        check_running_configuration(plan)
    value = state(plan)
    owner = str(uuid.uuid5(uuid.UUID(plan["deployment_id"]), "auth-" + mode))
    resource = business.resources(owner)
    password, credential_hash = business.random_credentials()
    resource["credential_fingerprint"] = hashlib.md5(credential_hash.encode()).hexdigest()
    previous = value.get("auth_fixtures", {}).get(mode)
    require(previous is None or previous["cleanup_complete"], "auth_fixture_needs_cleanup")
    business.assert_absent(sql, resource)
    value.setdefault("auth_fixtures", {})[mode] = {"resource": resource, "cleanup_complete": False, "absent_before": True}
    atomic(state_path(plan), value)
    fixture = {"identifier": resource["email"], "password": password}
    business.seed(sql, resource, credential_hash=credential_hash)
    value = state(plan)
    value["auth_fixtures"][mode]["seeded"] = True
    atomic(state_path(plan), value)
    try:
        if mode == "candidate":
            auth.check_candidate(LOCK)
            cert = Path("/etc/letsencrypt/live") / auth.HOST
            with auth.candidate_tls(18383, cert / "fullchain.pem", cert / "privkey.pem") as port:
                auth.run_smoke(f"https://{auth.HOST}:{port}", 18383, fixture, loopback=True)
        else:
            expected = plan["artifact_sha256"] if mode == "formal" else plan["baseline"]["service"]["file_sha256"]
            pid = auth.check_formal(expected)
            auth.run_smoke(f"https://{auth.HOST}", 8383, fixture)
            require(auth.check_formal(expected) == pid, "formal_process_changed")
    finally:
        clean_fixture(plan, "auth_fixtures", mode)


def business_smoke(plan):
    check_running_configuration(plan)
    business.check_schema(lambda statement: sql(statement, readonly=True))
    resource = business.resources(plan["deployment_id"])
    password, credential_hash = business.random_credentials()
    resource["credential_fingerprint"] = hashlib.md5(credential_hash.encode()).hexdigest()
    value = state(plan)
    require(value.get("business") is None, "business_smoke_already_attempted")
    business.assert_absent(sql, resource)
    value["business"] = {"resource": resource, "cleanup_complete": False, "started": True, "absent_before": True}
    atomic(state_path(plan), value)
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=ssl.create_default_context()),
                                        urllib.request.HTTPCookieProcessor(jar), auth.NoRedirect())
    def request(method, path, body, token):
        headers = {"Content-Type": "application/json", "X-TSZ-Wordlist-Custom-Items": "v1", "X-TSZ-Wordlist-Annotations": "v1"}
        if token:
            headers["Authorization"] = "Bearer " + token
        req = urllib.request.Request("https://" + auth.HOST + path, method=method, headers=headers,
                                     data=json.dumps(body).encode() if body is not None else None)
        try:
            response = opener.open(req, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            require(response.status == 200, "business_http_failed")
            return json.loads(response.read(1048576))
    failure = None
    try:
        business.seed(sql, resource, credential_hash=credential_hash)
        business.verify(request, resource, password=password)
    except BaseException as error:
        failure = error
    finally:
        try:
            clean_fixture(plan, "business")
        except BaseException:
            raise HostError("business_cleanup_failed") from None
    if failure is not None:
        raise HostError("business_verification_failed") from failure


def cleanup_business(plan):
    value = state(plan)
    for mode, fixture in value.get("auth_fixtures", {}).items():
        if not fixture["cleanup_complete"]:
            clean_fixture(plan, "auth_fixtures", mode)
    if value.get("business") and not value["business"]["cleanup_complete"]:
        clean_fixture(plan, "business")


def unlock(plan):
    owner_check(plan)
    # An atomic rename releases the active marker without a half-deleted owner window.
    # Keep the released owner next to retained state/backups for interrupted-response recovery.
    for path in LOCK.iterdir():
        require(path.name in {"owner", "git_sha", "candidate_pid"}, "unexpected_lock_content")
    released = transaction(plan) / "released-lock"
    require(not released.exists(), "released_lock_already_exists")
    LOCK.rename(released)


def recover(plan, credentials):
    value = state(plan)
    require(configuration_identity(configuration()) == value["configuration_sha256"], "deployment_configuration_changed")
    stop_candidate(plan)
    run(["systemctl", "stop", "tsz-rust"])
    cleanup_business(plan)
    actual = ledger()
    original = plan["baseline"]["postgresql"]["migration_ledger"]
    if actual != original:
        require(all(row.get("success") is True for row in actual), "dirty_migration_ledger")
        minimum = plan["migration"]["baseline_version"]
        require([row for row in actual if row["version"] <= minimum] == original, "baseline_migration_drift")
        expected = {row["up"]["version"]: row["up"]["checksum"] for row in plan["migration"]["migrations"]}
        added = [row for row in actual if row["version"] > minimum]
        require(added and all(expected.get(row["version"]) == row["checksum"] for row in added), "unexpected_recovery_migration")
        require([row["version"] for row in added] == plan["migration"]["versions"][:len(added)], "recovery_migration_gap")
        maximum = max(row["version"] for row in actual)
        require(minimum < maximum <= plan["migration"]["candidate_version"], "migration_outside_recovery_interval")
        require((plan.get("recovery_policy") or {}).get("kind") == "verified_down", "manual_migration_recovery_required")
        require(sha(BINARY) == plan["artifact_sha256"], "candidate_binary_required_for_recovery")
        run([str(BINARY), "deploy-undo-migrations", str(minimum), str(maximum)], environment=process_environment(plan), timeout=120)
        require(ledger() == original, "recovery_ledger_mismatch")
    manifests.restore_deployment(backup_dir=transaction(plan) / "backup", artifact=BINARY, manifest_path=MANIFEST)
    start_formal(plan)
    wait_formal(plan["baseline"]["service"]["file_sha256"])
    auth_smoke(plan, "recovery", credentials)
    manifests.verify_manifest(MANIFEST, BINARY)
    value = state(plan)
    value.update(recovered=True, accepted=False, candidate_pid=None)
    atomic(state_path(plan), value)
    unlock(plan)


def action(name, plan, credentials):
    value = state(plan)
    if name == "backup":
        backup(plan)
    elif name == "stop":
        baseline_matches(plan)
        value["mutation_started"] = True
        atomic(state_path(plan), value)
        run(["systemctl", "stop", "tsz-rust"])
        require(int(run(["systemctl", "show", "tsz-rust", "-p", "MainPID", "--value"]).strip()) == 0, "formal_service_not_stopped")
    elif name == "install":
        require(sha(transaction(plan) / "incoming") == plan["artifact_sha256"], "uploaded_digest_mismatch")
        MANIFEST.unlink(missing_ok=True)
        manifests._atomic_replace_file(transaction(plan) / "incoming", BINARY)
        BINARY.chmod(0o755)
        require(sha(BINARY) == plan["artifact_sha256"], "installed_digest_mismatch")
    elif name == "candidate_start":
        start_candidate(plan)
    elif name == "candidate_verify":
        auth_smoke(plan, "candidate", credentials)
        require(ledger() == [{"version": row["version"], "checksum": row["checksum"], "success": True}
                             for row in sorted(json.loads(run([str(BINARY), "deploy-info"]).decode())["migrations"], key=lambda r: r["version"])
                             if row["direction"] == "up"], "candidate_migration_ledger_mismatch")
    elif name == "formal_start":
        stop_candidate(plan)
        start_formal(plan)
        wait_formal(plan["artifact_sha256"])
    elif name == "formal_verify":
        auth_smoke(plan, "formal", credentials)
    elif name == "business_verify":
        business_smoke(plan)
    elif name == "accept":
        fixtures = state(plan).get("auth_fixtures", {})
        require(state(plan).get("business", {}).get("cleanup_complete")
                and all(fixtures.get(mode, {}).get("cleanup_complete") for mode in ("candidate", "formal")), "business_cleanup_not_complete")
        auth.check_formal(plan["artifact_sha256"])
        evidence = {key: plan["ci"][key] for key in ("run_attempt", "artifact_id", "verification_attempt", "verification_receipt_sha256")}
        evidence["artifact_sha256"] = plan["artifact_sha256"]
        manifests.create_manifest(component="api", repository="LonelyFellas/tsz-rust", git_sha=plan["source"]["git_sha"],
                                  git_tree=plan["source"]["git_tree"], ci_run_id=plan["ci"]["run_id"], ci_run_url=plan["ci"]["run_url"],
                                  artifact=BINARY, artifact_path=manifests.EXPECTED_ARTIFACT_PATH, output=MANIFEST, release_evidence=evidence)
        value = state(plan)
        value["accepted"] = True
        atomic(state_path(plan), value)
    elif name == "unlock":
        require(state(plan).get("accepted"), "deployment_not_accepted")
        unlock(plan)
    else:
        raise HostError("unsupported_action")
    if name != "unlock":
        value = state(plan)
        value["completed"].append(name)
        value["pending"] = None
        atomic(state_path(plan), value)
    return {"done": name}


def observe(plan):
    folder = transaction(plan)
    if not folder.exists() and not LOCK.exists():
        baseline_matches(plan)
        return {"owner": None, "mutation_started": False, "accepted": False}
    require(folder.is_dir(), "remote_transaction_missing")
    with (folder / "operation.lock").open("r+") as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise HostError("remote_operation_busy") from None
        value = state(plan)
        owner = (LOCK / "owner").read_text().strip() if LOCK.exists() else None
        accepted = MANIFEST.exists() and json.loads(MANIFEST.read_text()).get("source", {}).get("git_sha") == plan["source"]["git_sha"]
        matches = False
        if accepted:
            try:
                m = manifests.verify_manifest(MANIFEST, BINARY)
                matches = accepted_identity(plan, m, value)
                if matches:
                    auth.check_formal(plan["artifact_sha256"])
            except (OSError, ValueError, manifests.ManifestError, auth.SmokeFailure):
                matches = False
        recovered = value.get("recovered", False)
        recovery_matches = False
        if recovered:
            try:
                recovery_matches = (json.loads(MANIFEST.read_text()) == plan["baseline"]["deployment_manifest"]
                                    and ledger() == plan["baseline"]["postgresql"]["migration_ledger"])
                auth.check_formal(plan["baseline"]["service"]["file_sha256"])
            except (OSError, ValueError, HostError, auth.SmokeFailure):
                recovery_matches = False
        return {"owner": owner, "accepted": accepted, "acceptance_matches": matches,
                "recovered": recovered, "recovery_matches": recovery_matches,
                "mutation_started": value["mutation_started"]}


def dispatch(name, plan, credentials):
    if name == "preflight":
        return preflight(plan)
    if name == "observe":
        return observe(plan)
    if name == "abort" and not LOCK.exists():
        baseline_matches(plan)
        return {"aborted": True}
    if name == "finish_unlock" and not LOCK.exists():
        observed = observe(plan)
        require(observed.get("acceptance_matches") or observed.get("recovery_matches"), "terminal_state_mismatch")
        return {"unlocked": True}
    if name == "acquire":
        require(not LOCK.exists(), "remote_deployment_busy")
        baseline_matches(plan)
        configuration_sha256 = check_running_configuration()
        transaction(plan).mkdir(mode=0o700, parents=True, exist_ok=False)
        (transaction(plan) / "operation.lock").touch(mode=0o600)
        atomic(state_path(plan), {"owner": plan["deployment_id"], "sha": plan["source"]["git_sha"],
                                 "configuration_sha256": configuration_sha256,
                                 "plan_sha256": hashlib.sha256(json.dumps(plan, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
                                 "mutation_started": False, "accepted": False, "completed": [], "pending": None})
        LOCK.mkdir(mode=0o700)
        (LOCK / "owner").write_text(plan["deployment_id"])
        (LOCK / "git_sha").write_text(plan["source"]["git_sha"])
        with operation(plan):
            baseline_matches(plan)
        return {"acquired": True}
    with operation(plan):
        if name == "recheck_baseline":
            baseline_matches(plan)
            return {"matches": True}
        if name == "recover":
            recover(plan, credentials)
            return {"recovered": True}
        if name == "abort":
            require(not state(plan)["mutation_started"], "mutation_already_started")
            baseline_matches(plan)
            unlock(plan)
            return {"aborted": True}
        if name == "finish_unlock":
            if state(plan).get("recovered"):
                require(json.loads(MANIFEST.read_text()) == plan["baseline"]["deployment_manifest"], "recovery_manifest_mismatch")
                auth.check_formal(plan["baseline"]["service"]["file_sha256"])
                require(ledger() == plan["baseline"]["postgresql"]["migration_ledger"], "recovery_ledger_mismatch")
            else:
                require(observe_unlocked_acceptance(plan), "acceptance_mismatch")
            unlock(plan)
            return {"unlocked": True}
        value = state(plan)
        steps = ("backup", "upload", "stop", "install", "candidate_start", "candidate_verify", "formal_start", "formal_verify", "business_verify", "accept", "unlock")
        require(name in steps and len(value["completed"]) < len(steps)
                and value["completed"] == list(steps[:len(value["completed"])])
                and steps[len(value["completed"])] == name, "forward_step_out_of_order")
        value["pending"] = name
        atomic(state_path(plan), value)
        return action(name, plan, credentials)


def accepted_identity(plan, manifest, value):
    evidence = {key: plan["ci"][key] for key in ("run_id", "run_url", "run_attempt", "artifact_id", "verification_attempt", "verification_receipt_sha256")}
    evidence.update(workflow="CI", conclusion="success", artifact_sha256=plan["artifact_sha256"])
    expected_ledger = plan["baseline"]["postgresql"]["migration_ledger"] + [
        {"version": row["up"]["version"], "checksum": row["up"]["checksum"], "success": True}
        for row in plan["migration"]["migrations"]]
    fixtures = value.get("auth_fixtures", {})
    return (manifest["schema_version"] == 2 and manifest["source"]["git_sha"] == plan["source"]["git_sha"]
            and manifest["source"]["git_tree"] == plan["source"]["git_tree"]
            and manifest["ci"] == evidence and manifest["artifact"]["sha256"] == plan["artifact_sha256"]
            and value["completed"][:9] == ["backup", "upload", "stop", "install", "candidate_start", "candidate_verify", "formal_start", "formal_verify", "business_verify"]
            and value.get("business", {}).get("cleanup_complete") is True
            and all(fixtures.get(mode, {}).get("cleanup_complete") is True for mode in ("candidate", "formal"))
            and ledger() == expected_ledger)


def observe_unlocked_acceptance(plan):
    m = manifests.verify_manifest(MANIFEST, BINARY)
    auth.check_formal(plan["artifact_sha256"])
    return accepted_identity(plan, m, state(plan))


def receive(plan, source):
    try:
        with operation(plan):
            value = state(plan)
            require(not value["mutation_started"] and value["completed"] == ["backup"], "upload_out_of_order")
            value["pending"] = "upload"
            atomic(state_path(plan), value)
            output = transaction(plan) / "incoming"
            with output.open("xb") as destination:
                shutil.copyfileobj(source, destination)
                destination.flush()
                os.fsync(destination.fileno())
            output.chmod(0o755)
            require(sha(output) == plan["artifact_sha256"], "uploaded_digest_mismatch")
            value["completed"].append("upload")
            value["pending"] = None
            atomic(state_path(plan), value)
        print(json.dumps({"ok": True}))
    except Exception as error:
        print(json.dumps({"ok": False, "stage": str(error) if isinstance(error, HostError) else type(error).__name__}))
        raise SystemExit(1)


def entry(payload):
    try:
        result = dispatch(payload["action"], payload["plan"], payload.get("credentials"))
        print(json.dumps({"ok": True, "result": result}))
    except (HostError, auth.SmokeFailure, manifests.ManifestError, OSError, ValueError, KeyError) as error:
        stage = str(error) if isinstance(error, (HostError, auth.SmokeFailure)) else type(error).__name__
        print(json.dumps({"ok": False, "stage": stage}))
