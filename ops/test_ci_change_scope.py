import pathlib
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from ci_change_scope import classify, docs_only


class ChangeScopeTests(unittest.TestCase):
    def test_only_explicit_documentation_namespaces_are_eligible(self):
        self.assertTrue(docs_only(["docs/ci/plan.md", "docs/features/example/design.md"]))
        for path in ("src/a.rs", "migrations/a.sql", ".sqlx/a.json", "docs/openapi.json",
                     "ops/ci_change_scope.py", ".github/workflows/ci.yml", "Cargo.toml",
                     "docs/ci/../runtime.md", "docs/ci/test.py", ".agents/skills/deploy/SKILL.md"):
            self.assertFalse(docs_only(["docs/ci/plan.md", path]))
        self.assertFalse(docs_only([]))
        self.assertFalse(classify("push", {}))
        self.assertFalse(classify("workflow_dispatch", {}))

    def test_renaming_runtime_source_to_docs_does_not_hide_deletion(self):
        previous = os.getcwd()
        with tempfile.TemporaryDirectory() as directory:
            try:
                os.chdir(directory)
                def git(*args):
                    return subprocess.check_output(["git", *args], text=True).strip()
                git("init", "-q")
                git("config", "user.name", "test")
                git("config", "user.email", "test@example.invalid")
                pathlib.Path("source.rs").write_text("fn main() {}\n")
                git("add", ".")
                git("commit", "-qm", "baseline")
                base = git("rev-parse", "HEAD")
                pathlib.Path("docs/ci").mkdir(parents=True)
                pathlib.Path("source.rs").rename("docs/ci/source.md")
                git("add", "-A")
                git("commit", "-qm", "rename")
                event = {"pull_request": {"base": {"sha": base}, "head": {"sha": git("rev-parse", "HEAD")}}}
                self.assertFalse(classify("pull_request", event))
            finally:
                os.chdir(previous)

    def test_classifier_error_falls_back_to_full_ci(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "event").write_text("invalid json")
            subprocess.run([sys.executable, str(pathlib.Path(__file__).with_name("ci_change_scope.py"))],
                           env={**os.environ, "GITHUB_EVENT_NAME": "pull_request", "GITHUB_EVENT_PATH": str(root / "event"),
                                "GITHUB_OUTPUT": str(root / "output")}, check=True, capture_output=True)
            self.assertEqual((root / "output").read_text(), "docs_only=false\n")

    def test_summary_does_not_allow_skipped_runtime_without_successful_doc_classification(self):
        workflow = (pathlib.Path(__file__).resolve().parents[1] / ".github/workflows/ci.yml").read_text()
        job = workflow.split("  ci-summary:\n", 1)[1].split("  release-artifact:\n", 1)[0]
        script = job.split("        run: |\n", 1)[1]
        defaults = {"AUDIT_RESULT": "success", "QUALITY_RESULT": "success", "INVENTORY_RESULT": "success",
                    "SCOPE_RESULT": "success", "DOCS_ONLY": "false", "UNIT_DOC_RESULT": "success", "INTEGRATION_RESULT": "success"}
        docs = {"DOCS_ONLY": "true", "UNIT_DOC_RESULT": "skipped", "INTEGRATION_RESULT": "success"}
        for changes, expected in (({}, True), (docs, True), ({"UNIT_DOC_RESULT": "skipped"}, False),
                                  ({**docs, "SCOPE_RESULT": "failure"}, False),
                                  ({**docs, "INTEGRATION_RESULT": "skipped"}, False),
                                  ({**docs, "AUDIT_RESULT": "failure"}, False),
                                  ({**docs, "QUALITY_RESULT": "cancelled"}, False),
                                  ({**docs, "INVENTORY_RESULT": "failure"}, False)):
            result = subprocess.run(["bash", "-e", "-c", script], env={**os.environ, **defaults, **changes}, capture_output=True)
            self.assertEqual(result.returncode == 0, expected)


if __name__ == "__main__":
    unittest.main()
