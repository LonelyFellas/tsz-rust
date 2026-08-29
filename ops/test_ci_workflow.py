import pathlib
import re
import unittest


class CiWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        repository_root = pathlib.Path(__file__).resolve().parent.parent
        cls.workflow = (repository_root / ".github/workflows/ci.yml").read_text(
            encoding="utf-8"
        )

    def test_ci07_integration_matrix_is_parallel_and_complete(self) -> None:
        integration = self._job("integration")

        self.assertIn("fail-fast: false", integration)
        self.assertIn("module: [lexicon, admin, identity, platform]", integration)
        self.assertIn(
            "python3 ops/ci_test_modules.py run ${{ matrix.module }}", integration
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

    def test_ci09_summary_depends_on_every_required_job(self) -> None:
        summary = self._job("ci-summary")

        self.assertIn("name: Format, lint and test", summary)
        self.assertIn("needs: [quality, unit-doc, integration]", summary)
        self.assertIn("if: ${{ always() }}", summary)
        self.assertIn("needs.quality.result", summary)
        self.assertIn("needs['unit-doc'].result", summary)
        self.assertIn("needs.integration.result", summary)

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

    def test_ci12_quality_and_unit_doc_keep_every_original_gate(self) -> None:
        quality = self._job("quality")
        unit_doc = self._job("unit-doc")

        self.assertIn("cargo fmt --all -- --check", quality)
        self.assertIn("python3 -m unittest ops/test_deployment_manifest.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_test_modules.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_workflow.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_fingerprint.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_metrics.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_cargo_fetch.py", quality)
        self.assertIn("python3 -m unittest ops/test_release_artifact_manifest.py", quality)
        self.assertIn("python3 -m unittest ops/test_deployment_preflight.py", quality)
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
