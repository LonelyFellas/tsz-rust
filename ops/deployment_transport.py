"""Fixed GitHub/tshb-test adapters. Controllers pass JSON, never shell-expanded values."""

import hashlib
import json
from pathlib import Path
import subprocess

from deploy_test import DeploymentError
from migration_compatibility import migration_interval


ROOT = Path(__file__).resolve().parent
REPOSITORY = "LonelyFellas/tsz-rust"
MODULES = ("deployment_manifest", "deployment_auth_smoke", "deployment_business_smoke", "deployment_host")


def command(arguments, *, data=None, timeout=60):
    try:
        result = subprocess.run(arguments, input=data, capture_output=True, text=True,
                                timeout=timeout, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise DeploymentError("transport_unavailable") from error
    if result.returncode:
        raise DeploymentError("transport_failed")
    return result.stdout


class GithubGate:
    def api(self, path):
        return json.loads(command(["gh", "api", "repos/" + REPOSITORY + "/" + path]))

    def check(self, plan):
        sha = plan["source"]["git_sha"]
        branch = self.api("branches/main")
        if branch["commit"]["sha"] != sha:
            raise DeploymentError("main_advanced")
        cwd = ROOT.parent
        def git(*args):
            return command(["git", "-C", str(cwd), *args]).strip()
        if (git("branch", "--show-current") != "main" or git("rev-parse", "HEAD") != sha
                or git("rev-parse", "HEAD^{tree}") != plan["source"]["git_tree"]
                or git("status", "--porcelain")):
            raise DeploymentError("clean_exact_main_checkout_required")
        workflows = self.api("actions/workflows")
        ids = [row["id"] for row in workflows["workflows"] if row["path"] == ".github/workflows/ci.yml" and row["name"] == "CI"]
        runs = self.api("actions/runs?branch=main&head_sha=" + sha + "&per_page=100")["workflow_runs"]
        runs = [row for row in runs if row["workflow_id"] in ids and row["name"] == "CI"]
        if not runs:
            raise DeploymentError("exact_ci_missing")
        latest = max(runs, key=lambda row: (row["created_at"], row["id"]))
        if (latest["id"] != plan["ci"]["run_id"] or latest["run_attempt"] != plan["ci"]["latest_attempt"]
                or latest["status"] != "completed" or latest["conclusion"] != "success"
                or latest["head_sha"] != sha or latest["head_branch"] != "main"
                or latest["event"] not in {"push", "workflow_dispatch"}
                or latest["repository"]["full_name"] != REPOSITORY
                or latest["head_repository"]["full_name"] != REPOSITORY):
            raise DeploymentError("exact_ci_changed_or_failed")
        record = self.api("actions/artifacts/" + str(plan["ci"]["artifact_id"]))
        from release_artifact_smoke import verify_artifact_record
        from release_artifact_manifest import verify_manifest
        manifest = verify_manifest(Path(plan["manifest_path"]), Path(plan["artifact_path"]))
        verify_artifact_record(record, manifest, plan["ci"]["artifact_id"])
        local_record = json.loads(Path(plan["artifact_metadata_path"]).read_text())
        for key in ("id", "name", "digest", "expired", "workflow_run"):
            if record.get(key) != local_record.get(key):
                raise DeploymentError("artifact_metadata_changed")
        artifacts = self.api("actions/runs/" + str(latest["id"]) + "/artifacts?per_page=100")["artifacts"]
        receipt_name = "release-smoke-" + sha + "-attempt-" + str(plan["ci"]["latest_attempt"])
        receipts = [row for row in artifacts if row["name"] == receipt_name and not row["expired"]]
        if len(receipts) != 1:
            raise DeploymentError("latest_verification_receipt_missing")
        # Fetch only the named verification receipt into a new local directory, compare exact bytes.
        import tempfile
        with tempfile.TemporaryDirectory(prefix="tsz-ci-verification-") as directory:
            command(["gh", "run", "download", str(latest["id"]), "--repo", REPOSITORY,
                     "--name", receipt_name, "--dir", directory])
            files = list(Path(directory).rglob("release-smoke-receipt.json"))
            if len(files) != 1 or hashlib.sha256(files[0].read_bytes()).hexdigest() != plan["ci"]["verification_receipt_sha256"]:
                raise DeploymentError("verification_receipt_not_from_ci")
        rows = []
        for filename in git("ls-tree", "-r", "--name-only", sha, "--", "migrations").splitlines():
            direction = "up" if filename.endswith(".up.sql") else "down" if filename.endswith(".down.sql") else None
            if direction:
                raw = subprocess.check_output(["git", "-C", str(cwd), "show", sha + ":" + filename])
                rows.append({"version": int(Path(filename).name.split("_", 1)[0]), "direction": direction,
                             "checksum": hashlib.sha384(raw).hexdigest(),
                             "transactional": not raw.startswith(b"-- no-transaction")})
        expected = migration_interval({"schema_version": 1, "migrations": rows}, plan["baseline"]["postgresql"]["migration_ledger"])
        if expected != plan["migration"]:
            raise DeploymentError("migration_interval_differs_from_source")
        # Validate the protected runner's explicit recovery approval file, not a boolean in a plan.
        if expected["versions"]:
            policy = plan["recovery_policy"]
            approval = Path.home() / ".config/tsz-rust/recovery-approvals" / (policy["approval_evidence_sha256"] + ".json")
            if not approval.is_file() or hashlib.sha256(approval.read_bytes()).hexdigest() != policy["approval_evidence_sha256"]:
                raise DeploymentError("approved_recovery_evidence_missing")
            content = json.loads(approval.read_text())
            for key in ("baseline_sha", "candidate_sha", "baseline_version", "candidate_version", "kind"):
                if content.get(key) != policy[key]:
                    raise DeploymentError("approved_recovery_evidence_mismatch")
            if (content.get("baseline_artifact_sha256") != plan["baseline"]["service"]["file_sha256"]
                    or content.get("candidate_artifact_sha256") != plan["artifact_sha256"]
                    or content.get("migration_receipt_sha256") != hashlib.sha256(Path(plan["migration_receipt_path"]).read_bytes()).hexdigest()
                    or content.get("migration_interval_sha256") != hashlib.sha256(json.dumps(expected, sort_keys=True, separators=(",", ":")).encode()).hexdigest()):
                raise DeploymentError("recovery_approval_not_bound_to_evidence")


class SshHost:
    credentials = None

    def request(self, action, plan=None):
        payload = {"action": action, "plan": plan, "credentials": self.credentials,
                   "modules": {name: (ROOT / (name + ".py")).read_text() for name in MODULES}}
        bootstrap = "import json,sys,types; p=json.load(sys.stdin); " \
                    "[(sys.modules.setdefault(n,types.ModuleType(n))) for n in p['modules']]; " \
                    "[(exec(compile(s,n+'.py','exec'),sys.modules[n].__dict__)) for n,s in p['modules'].items()]; " \
                    "sys.modules['deployment_host'].entry(p)"
        # All arguments other than a fixed bootstrap travel via stdin, including credentials.
        import shlex
        text = command(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o",
                        "StrictHostKeyChecking=yes", "tshb-test", "python3 -I -c " + shlex.quote(bootstrap)],
                       data=json.dumps(payload), timeout=300)
        result = json.loads(text)
        if not result.get("ok"):
            raise DeploymentError(result.get("stage", "remote_action_failed"))
        return result.get("result", {})

    def baseline(self):
        # Reuse the standalone read-only stage-B collector; it does not create remote files.
        source = (ROOT / "deployed_baseline.py").read_text()
        result = json.loads(command(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o",
                                     "StrictHostKeyChecking=yes", "tshb-test", "python3 -I - --remote"], data=source))
        return result

    def preflight(self, plan):
        self.request("preflight", plan)

    def acquire(self, plan):
        self.request("acquire", plan)

    def recheck_baseline(self, plan):
        self.request("recheck_baseline", plan)

    def action(self, step, plan):
        if step == "upload":
            artifact = Path(plan["artifact_path"])
            # Stream trusted modules separately from the artifact bytes. Receiver imports
            # are satisfied in memory; downloaded files are never Python import sources.
            modules = {name: (ROOT / (name + ".py")).read_text() for name in MODULES}
            receiver = "import json,sys,types; p=json.loads(sys.stdin.buffer.readline()); " \
                       "[(sys.modules.setdefault(n,types.ModuleType(n))) for n in p['modules']]; " \
                       "[(exec(compile(s,n+'.py','exec'),sys.modules[n].__dict__)) for n,s in p['modules'].items()]; " \
                       "sys.modules['deployment_host'].receive(p['plan'],sys.stdin.buffer)"
            import shlex
            try:
                process = subprocess.Popen(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o",
                                            "StrictHostKeyChecking=yes", "tshb-test", "python3 -I -c " + shlex.quote(receiver)],
                                           stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                try:
                    payload = (json.dumps({"plan": plan, "modules": modules}) + "\n").encode() + artifact.read_bytes()
                    stdout, _ = process.communicate(input=payload, timeout=180)
                except BaseException:
                    process.kill()
                    process.wait()
                    raise
                if process.returncode or json.loads(stdout).get("ok") is not True:
                    raise DeploymentError("artifact_upload_failed")
            except (OSError, subprocess.TimeoutExpired, ValueError):
                raise DeploymentError("artifact_upload_failed") from None
        else:
            self.request(step, plan)

    def observe(self, plan):
        return self.request("observe", plan)

    def finish_unlock(self, plan):
        self.request("finish_unlock", plan)

    def abort_unmutated(self, plan):
        self.request("abort", plan)

    def recover(self, plan):
        self.request("recover", plan)
