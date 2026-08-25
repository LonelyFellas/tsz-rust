import hashlib
import pathlib
import re
import unittest


RELEASE_JOB_SHA256 = "976d981ac3b8bd585bc46a4c8c045f96c6c1dd926f5d5dfd4835253eb8290aef"


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

    def test_ci10_release_artifact_job_is_byte_for_byte_unchanged(self) -> None:
        release = self._job("release-artifact")

        self.assertEqual(
            hashlib.sha256(release.encode()).hexdigest(), RELEASE_JOB_SHA256
        )

    def test_ci12_quality_and_unit_doc_keep_every_original_gate(self) -> None:
        quality = self._job("quality")
        unit_doc = self._job("unit-doc")

        self.assertIn("cargo fmt --all -- --check", quality)
        self.assertIn("python3 -m unittest ops/test_deployment_manifest.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_test_modules.py", quality)
        self.assertIn("python3 -m unittest ops/test_ci_workflow.py", quality)
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
