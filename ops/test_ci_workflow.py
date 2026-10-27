import os
import pathlib
import re
import subprocess
import sys
import tempfile
import unittest


class CiWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        repository_root = pathlib.Path(__file__).resolve().parent.parent
        cls.workflow = (repository_root / ".github/workflows/ci.yml").read_text(
            encoding="utf-8"
        )

    def test_backup_restore_safety_checks_run_in_quality(self) -> None:
        self.assertIn(
            "python3 -m unittest discover -s ops -p 'test_backup_restore_drill.py'",
            self._job("quality"),
        )

    def test_git_hook_regressions_run_in_quality(self) -> None:
        self.assertIn(
            "python3 -m unittest ops/test_rule_doc_hook.py ops/test_pre_push_hook.py",
            self._job("quality"),
        )

    def test_ci07_integration_matrix_is_parallel_and_complete(self) -> None:
        integration = self._job("integration")

        self.assertIn("fail-fast: false", integration)
        self.assertIn("module: [lexicon-pagination, lexicon-handler, lexicon-other, admin, identity, platform]", integration)
        self.assertIn(
            "python3 ops/ci_identity_profile.py run ${{ matrix.module }}", integration
        )

    def test_ci08_only_test_jobs_start_database_services(self) -> None:
        quality = self._job("quality")
        unit_doc = self._job("unit-doc")
        integration = self._job("integration")

        self.assertNotIn("services:", quality)
        for job in (unit_doc, integration):
            with self.subTest(job=job.splitlines()[0]):
                self.assertIn("postgres:", job)
                self.assertIn("redis:", job)
                self.assertIn("DATABASE_URL:", job)
                self.assertIn("REDIS_URL:", job)

    def test_ci16_integration_exposes_explicit_test_redis_url(self) -> None:
        integration = self._job("integration")

        self.assertRegex(
            integration,
            r"(?m)^      TEST_REDIS_URL: redis://localhost:6379/0$",
        )

    def test_ci09_summary_depends_on_every_required_job(self) -> None:
        summary = self._job("ci-summary")

        self.assertIn("name: Format, lint and test", summary)
        self.assertIn("needs: [quality, unit-doc, integration, lexicon-summary, dependency-audit]", summary)
        self.assertIn("if: ${{ always() && github.event_name != 'schedule' }}", summary)
        self.assertIn("needs['dependency-audit'].result", summary)
        self.assertIn('test "$AUDIT_RESULT" = "success"', summary)
        self.assertIn("needs.quality.result", summary)
        self.assertIn("needs['unit-doc'].result", summary)
        self.assertIn("needs.integration.result", summary)
        inventory = self._job("lexicon-summary")
        self.assertIn("name: Integration (lexicon)", inventory)
        self.assertIn("needs: [integration]", inventory)
        self.assertIn("ci_test_shards.py verify integration-inventories", inventory)
        self.assertIn("pattern: integration-inventory-*", inventory)
        self.assertIn("needs['lexicon-summary'].result", summary)
        self.assertIn("if-no-files-found: error", self._job("integration"))
        self.assertIn("if: matrix.module == 'identity'", self._job("integration"))
        self.assertIn("name: identity-phase-measurements", self._job("integration"))
        self.assertIn("if: matrix.module == 'lexicon-handler'", self._job("integration"))
        self.assertIn("name: lexicon-handler-phase-measurements", self._job("integration"))

    def test_dependency_audit_blocks_ci_and_runs_alone_on_schedule(self) -> None:
        audit = self._job("dependency-audit")
        self.assertIn("cargo install cargo-audit --version 0.22.2 --locked", audit)
        self.assertIn("cargo audit --deny warnings", audit)
        self.assertNotIn("if:", audit)
        self.assertNotIn("continue-on-error:", audit)
        self.assertNotIn("services:", audit)
        self.assertIn('cron: "23 3 * * 1"', self.workflow)
        self.assertIn("github.event_name == 'schedule' && 'scheduled-audit'", self.workflow)
        quality = self._job("quality")
        self.assertIn("needs: [dependency-audit]", quality)
        self.assertIn("always()", quality)
        self.assertIn("needs['dependency-audit'].result", quality)
        self.assertIn('test "$AUDIT_RESULT" = "success"', quality)
        self.assertLess(quality.index("Require dependency audit to pass"), quality.index("Check out repository"))
        for name in ("quality", "unit-doc", "integration", "lexicon-summary", "ci-summary"):
            self.assertIn("github.event_name != 'schedule'", self._job(name))

    def test_ci10_release_artifact_is_exact_main_and_auditable(self) -> None:
        release = self._job("release-artifact")

        self.assertIn("github.repository == 'LonelyFellas/tsz-rust'", release)
        self.assertIn("github.ref == 'refs/heads/main'", release)
        self.assertNotIn("github.event_name == 'pull_request'", release)
        self.assertIn("tsz-rust.manifest.json", release)
        self.assertIn("${{ github.sha }}", release)
        self.assertIn("${{ github.run_attempt }}", release)
        self.assertIn("release_artifact_manifest.py create", release)
        self.assertIn("release_artifact_manifest.py verify", release)

    def test_release_smoke_downloads_exact_attempt_and_never_deploys(self) -> None:
        smoke = self._job("release-smoke")
        self.assertIn("needs: [release-artifact]", smoke)
        self.assertIn("actions: read", smoke)
        self.assertIn("contents: read", smoke)
        self.assertIn("github.ref == 'refs/heads/main'", smoke)
        self.assertIn("github.event_name == 'push' || github.event_name == 'workflow_dispatch'", smoke)
        self.assertIn("artifact-ids: ${{ needs.release-artifact.outputs.artifact_id }}", smoke)
        self.assertIn("--artifact-id '${{ needs.release-artifact.outputs.artifact_id }}'", smoke)
        self.assertIn('--expected-sha "$GITHUB_SHA"', smoke)
        self.assertIn("--run-attempt '${{ needs.release-artifact.outputs.producer_attempt }}'", smoke)
        self.assertIn('--verification-attempt "$GITHUB_RUN_ATTEMPT"', smoke)
        self.assertIn('actions/artifacts/$ARTIFACT_ID', smoke)
        self.assertNotIn('attempt-${GITHUB_RUN_ATTEMPT}', smoke)
        release = self._job("release-artifact")
        self.assertIn("artifact_id: ${{ steps.upload.outputs.artifact-id }}", release)
        self.assertIn("producer_attempt: ${{ steps.source.outputs.producer_attempt }}", release)
        self.assertIn("git_tree: ${{ steps.source.outputs.git_tree }}", release)
        self.assertIn('echo "producer_attempt=$GITHUB_RUN_ATTEMPT"', release)
        self.assertIn("id: upload", release)
        self.assertIn("release_artifact_smoke.py", smoke)
        self.assertIn("docker pull postgres:18-alpine", smoke)
        self.assertIn("docker pull redis:6.0-alpine", smoke)
        self.assertIn("docker pull --platform linux/amd64 ubuntu:22.04", smoke)
        self.assertIn("if: ${{ always() }}", smoke)
        self.assertIn("if-no-files-found: error", smoke)
        self.assertNotIn("continue-on-error", smoke)
        self.assertNotIn("ssh ", smoke)
        self.assertNotIn("secrets.", smoke)
        self.assertNotIn("workflow_run:", self.workflow)
        quality = self._job("quality")
        self.assertIn("test_release_artifact_smoke.py", quality)
        self.assertIn("test_migration_compatibility.py", quality)
        self.assertIn("test_deployed_baseline.py", quality)

    def test_ci11_caches_are_explicit_restore_only_outside_trusted_main(self) -> None:
        jobs = [
            self._job("quality"),
            self._job("unit-doc"),
            self._job("integration"),
            self._job("release-artifact"),
        ]

        for job in jobs:
            with self.subTest(job=job.splitlines()[0]):
                self.assertIn("id: cargo-cache", job)
                self.assertLess(job.index("id: ci-fingerprint"), job.index("id: cargo-cache"))
                self.assertIn("continue-on-error: true", job)
                self.assertIn("cache-bin: false", job)
                self.assertIn("cache-targets: false", job)
                self.assertIn("cache-on-failure: false", job)
                self.assertIn("steps.ci-fingerprint.outputs.cache_key", job)
                self.assertIn("github.event_name == 'push'", job)
                self.assertIn("github.ref == 'refs/heads/main'", job)
                self.assertIn("github.repository == 'LonelyFellas/tsz-rust'", job)
                self.assertIn("ops/ci_cargo_fetch.py", job)

    def test_ci13_phase_zero_metrics_cover_every_cargo_gate(self) -> None:
        quality = self._job("quality")
        unit_doc = self._job("unit-doc")
        integration = self._job("integration")
        release = self._job("release-artifact")

        self.assertIn("--name clippy", quality)
        self.assertIn("--name lib-bins", unit_doc)
        self.assertIn("--name doc-tests", unit_doc)
        self.assertIn("--name integration-${{ matrix.module }}", integration)
        self.assertIn("--name integration-compile-${{ matrix.module }}", integration)
        self.assertLess(
            integration.index("ci_identity_profile.py build"),
            integration.index("ci_identity_profile.py run"),
        )
        self.assertIn("--name release-build", release)
        for job in (quality, unit_doc, integration, release):
            self.assertIn("--name cargo-fetch", job)
            self.assertIn("--name cargo-cache-hit", job)

    def test_ci14_workflow_does_not_promote_test_results_or_secrets(self) -> None:
        self.assertNotIn("pull_request_target:", self.workflow)
        self.assertNotIn("workflow_run:", self.workflow)
        self.assertNotRegex(self.workflow, r"(?i)cache.*(test[-_ ]?pass|test[-_ ]?result)")
        self.assertNotRegex(self.workflow, r"(?i)(database_url|redis_url).*cache")

    def test_ci15_release_container_marks_workspace_safe_before_git_checks(self) -> None:
        release = self._job("release-artifact")
        safe_directory = (
            'git config --global --add safe.directory "$GITHUB_WORKSPACE"'
        )

        self.assertIn(safe_directory, release)
        self.assertLess(
            release.index(safe_directory),
            release.index('test "$(git rev-parse HEAD)" = "$GITHUB_SHA"'),
        )

    def test_release_downloads_have_bounded_mirror_fallback(self) -> None:
        release = self._job("release-artifact")
        match = re.search(
            r"(?ms)^      - name: Install build dependencies\n.*?^        run: \|\n(.*?)(?=^      - name:)",
            release,
        )
        self.assertIsNotNone(match)
        script = "\n".join(line[10:] for line in match.group(1).splitlines())
        self.assertIn("--download-only", script)
        self.assertIn("--no-download", script)
        self.assertIn("APT::Update::Error-Mode=any", script)
        self.assertNotIn("mirror+file:", script)

        for failure in ("none", "update", "download", "all"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                sources = root / "sources.list"
                sources.write_text("deb http://archive.ubuntu.com/ubuntu/ jammy main\n")
                log = root / "calls"
                archives = root / "default-archives"
                (archives / "partial").mkdir(parents=True)
                timeout = root / "timeout"
                timeout.write_text(
                    "#!/bin/sh\n"
                    'printf "timeout %s %s\\n" "$1" "$2" >> "$CALL_LOG"\n'
                    "shift 2\n"
                    'exec "$@"\n'
                )
                timeout.chmod(0o755)
                apt = root / "apt-get"
                apt.write_text(
                    "#!/bin/sh\n"
                    'source=$(cut -d " " -f 2 "$APT_SOURCES")\n'
                    'printf "%s %s\\n" "$source" "$*" >> "$CALL_LOG"\n'
                    'archives="$DEFAULT_ARCHIVES"\n'
                    'for arg in "$@"; do\n'
                    '  case "$arg" in Dir::Cache::archives=*) archives=${arg#*=} ;; esac\n'
                    "done\n"
                    'case "$*" in\n'
                    '  *update) rm -f "$DEFAULT_ARCHIVES"/*.deb "$DEFAULT_ARCHIVES"/partial/*.deb ;;\n'
                    '  *--no-download*) test -f "$archives/complete.deb"; exit $? ;;\n'
                    "esac\n"
                    '[ "$FAILURE" != all ] || exit 100\n'
                    'case "$source:$FAILURE:$*" in\n'
                    "  *azure.archive*:update:*update) exit 124 ;;\n"
                    '  *azure.archive*:download:*--download-only*) touch "$archives/progress.deb"; exit 124 ;;\n'
                    '  *:download:*--download-only*) test -f "$archives/progress.deb" || exit 100 ;;\n'
                    "esac\n"
                    'case "$*" in *--download-only*) touch "$archives/complete.deb" ;; esac\n'
                )
                apt.chmod(0o755)
                sed = root / "sed"
                sed.write_text(
                    f"#!{sys.executable}\n"
                    "import subprocess, sys\n"
                    "args = sys.argv[1:]\n"
                    "if sys.platform == 'darwin': args.insert(1, '')\n"
                    "raise SystemExit(subprocess.call(['/usr/bin/sed', *args]))\n"
                )
                sed.chmod(0o755)
                result = subprocess.run(
                    ["/bin/sh"],
                    input=script.replace("/etc/apt/sources.list", str(sources)),
                    text=True,
                    capture_output=True,
                    env={
                        **os.environ,
                        "PATH": f"{root}:{os.environ['PATH']}",
                        "CALL_LOG": str(log),
                        "APT_SOURCES": str(sources),
                        "DEFAULT_ARCHIVES": str(archives),
                        "TMPDIR": str(root),
                        "FAILURE": failure,
                    },
                    timeout=10,
                )
                calls = log.read_text().splitlines()
                self.assertTrue(all(
                    line in ("timeout --kill-after=5s 30s", "timeout --kill-after=5s 90s")
                    for line in calls if line.startswith("timeout ")
                ))
                downloads = [line for line in calls if "--download-only" in line]
                installs = [line for line in calls if "--no-download" in line]
                if failure == "all":
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(installs, [])
                    self.assertEqual(len([line for line in calls if line.endswith(" update")]), 3)
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(len(installs), 1)
                    self.assertIn("--download-only", calls[-2])
                    self.assertIn("--no-download", calls[-1])
                    self.assertEqual(len(downloads), 2 if failure == "download" else 1)
                    expected_host = "azure.archive.ubuntu.com" if failure == "none" else "archive.ubuntu.com"
                    self.assertTrue(installs[0].startswith(f"http://{expected_host}/ubuntu/ "))

    def test_ci12_quality_and_unit_doc_keep_every_original_gate(self) -> None:
        quality = self._job("quality")
        unit_doc = self._job("unit-doc")

        self.assertIn("cargo fmt --all -- --check", quality)
        self.assertIn("python3 -m unittest ops/test_deployment_manifest.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_test_modules.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_test_shards.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_identity_profile.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_workflow.py", quality)
        self.assertIn(
            "python3 -I ops/speech-voice-catalog/test_update_metadata.py", quality
        )
        self.assertIn("python3 -m unittest ops/test_ci_fingerprint.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_metrics.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_cargo_fetch.py", quality)
        self.assertIn("python3 -m unittest ops/test_release_artifact_manifest.py", quality)
        self.assertIn("python3 -m unittest ops/test_deployment_preflight.py", quality)
        for target in ("test_deploy_test.py", "test_deployment_host.py", "test_deployment_transport.py", "test_deployment_business_smoke.py", "test_deployment_transaction.py"):
            self.assertIn(target, quality)
        self.assertIn("python3 -m unittest ops/test_deploy_skill.py", quality)
        self.assertIn(
            "cargo clippy --locked --all-targets --all-features -- -D warnings",
            quality,
        )
        self.assertIn(
            "cargo test --locked --all-features --lib --bins", unit_doc
        )
        self.assertIn("cargo test --locked --all-features --doc", unit_doc)

    def _job(self, name: str) -> str:
        match = re.search(
            rf"(?ms)^  {re.escape(name)}:\n.*?(?=^  [a-zA-Z0-9_-]+:\n|\Z)",
            self.workflow,
        )
        self.assertIsNotNone(match, f"job not found: {name}")
        return match.group(0)


if __name__ == "__main__":
    unittest.main()
