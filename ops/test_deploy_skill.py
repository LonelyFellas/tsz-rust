import pathlib
import re
import subprocess
import tempfile
import textwrap
import unittest


class DeploySkillTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = pathlib.Path(__file__).resolve().parent.parent
        cls.compatibility_entry = cls.root / ".claude/skills/deploy/SKILL.md"
        cls.project_entry = cls.root / ".agents/skills/deploy/SKILL.md"
        cls.runbook_path = cls.project_entry.parent / "references/runbook.md"
        cls.runbook = cls.runbook_path.read_text(encoding="utf-8")

    def test_skill_entries_resolve_to_one_artifact_contract(self) -> None:
        for entry, expected in (
            (self.compatibility_entry, self.project_entry),
            (self.project_entry, self.runbook_path),
        ):
            links = re.findall(r"\]\(([^)]+)\)", entry.read_text(encoding="utf-8"))
            targets = {(entry.parent / link).resolve() for link in links}
            self.assertIn(expected.resolve(), targets)
        self.assertIn("tsz-rust-x86_64-linux-gnu-$deploy_sha", self.runbook)
        self.assertIn("attempt-$ci_run_attempt", self.runbook)
        self.assertIn("artifact_name\" =~ ^tsz-rust", self.runbook)
        self.assertNotIn("--jq --arg", self.runbook)
        self.assertIn('--jq ".artifacts[] | select(.name == \\"$artifact_name\\")', self.runbook)
        self.assertIn('"$tools/release_artifact_manifest.py" verify', self.runbook)
        self.assertIn("--expected-run-attempt", self.runbook)
        self.assertNotIn(
            "--name tsz-rust-x86_64-linux-gnu --dir", self.runbook
        )

    def test_skill_uses_machine_preflight_and_reports_deploy_timings(self) -> None:
        self.assertIn('< "$tools/deployment_preflight.py"', self.runbook)
        self.assertIn("--name deploy-artifact-download", self.runbook)
        self.assertIn("--name deploy-binary-rsync", self.runbook)
        self.assertIn("--name deploy-server-build", self.runbook)
        self.assertNotIn("cargo build --release", self.runbook)

    def test_artifact_is_verified_before_any_server_mutation(self) -> None:
        download = self.runbook.index("--name deploy-artifact-download")
        server_lock = self.runbook.index("server_lock_result=")
        backup = self.runbook.index("deploy_backup_dir=$(ssh")
        self.assertLess(download, server_lock)
        self.assertLess(server_lock, backup)

    def test_state_staging_and_server_mutations_are_session_owned(self) -> None:
        self.assertIn("~/.config/tsz-rust/deploy.lock/state.env", self.runbook)
        self.assertIn("~/.cache/tsz-rust-artifacts", self.runbook)
        self.assertIn("/opt/tsz-rust/deploy.lock/owner", self.runbook)
        self.assertIn("$deploy_session", self.runbook)
        self.assertNotIn("~/.config/tsz-rust/deploy-state.env", self.runbook)
        self.assertNotIn("~/.cache/tsz-rust-artifact\n", self.runbook)
        self.assertNotIn('rm -rf "$staging"', self.runbook)

    def test_every_bash_block_is_fail_fast_and_pipe_safe(self) -> None:
        bash_blocks = re.findall(
            r"(?ms)^\s*```bash\n(.*?)^\s*```$", self.runbook
        )
        self.assertGreater(len(bash_blocks), 0)
        for block in bash_blocks:
            with self.subTest(block=block.splitlines()[1:3]):
                self.assertEqual(block.splitlines()[0].strip(), "set -euo pipefail")
                syntax = subprocess.run(
                    ["bash", "-n"],
                    input=textwrap.dedent(block),
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertEqual(syntax.returncode, 0, syntax.stderr)

    def test_owner_mismatch_cannot_reach_a_following_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marker = pathlib.Path(directory) / "mutated"
            completed = subprocess.run(
                [
                    "bash",
                    "-c",
                    'set -euo pipefail; test "$ACTUAL" = "$EXPECTED"; : > "$MARKER"',
                ],
                check=False,
                env={
                    "ACTUAL": "other-session",
                    "EXPECTED": "current-session",
                    "MARKER": str(marker),
                },
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertFalse(marker.exists())

        self.assertGreaterEqual(
            self.runbook.count('ssh tshb-test "set -eu; test'), 2
        )

    def test_remote_values_are_validated_or_shell_escaped_before_state_append(self) -> None:
        self.assertIn("manifest git_sha 非法", self.runbook)
        self.assertIn("deploy_backup_dir\" =~", self.runbook)
        self.assertIn("printf 'deployed_sha=%q", self.runbook)
        self.assertIn("printf 'deploy_backup_dir=%q", self.runbook)
        self.assertNotIn("deployed_sha=$deployed_sha\n", self.runbook)
        self.assertNotIn("deploy_backup_dir=$deploy_backup_dir\n", self.runbook)

    def test_owner_checks_share_the_remote_mutation_command(self) -> None:
        self.assertNotIn('ssh tshb-test "test', self.runbook)
        self.assertIn(
            'deploy_tool_incoming="/opt/tsz-deploy-tools/'
            'backend-deployment-manifest.incoming.$deploy_session"',
            self.runbook,
        )
        self.assertIn(
            'binary_incoming="/opt/tsz-rust/target/release/'
            'tsz-rust.incoming.$deploy_session"',
            self.runbook,
        )
        for mutation in (
            "base=/opt/tsz-rust/deploy-backups",
            "rm -f /opt/tsz-deploy-manifests/api.json",
            "mv -f '$binary_incoming'",
            "systemctl restart tsz-rust",
            "backend-deployment-manifest.py create",
            "backend-deployment-manifest.py restore",
        ):
            with self.subTest(mutation=mutation):
                position = self.runbook.index(mutation)
                command_start = self.runbook.rfind("ssh tshb-test", 0, position)
                owner_check = self.runbook.find("deploy.lock/owner", command_start, position)
                self.assertGreaterEqual(owner_check, command_start)

    def test_deploy_tools_are_extracted_from_the_verified_git_sha(self) -> None:
        self.assertIn(
            'git show "$deploy_sha:.agents/skills/deploy/scripts/require-green-main.sh"',
            self.runbook,
        )
        self.assertIn('git show "$deploy_sha:$tool_path"', self.runbook)
        for repository_path in (
            "ops/ci_fingerprint.py",
            "ops/ci_metrics.py",
            "ops/deployment_manifest.py",
            "ops/deployment_preflight.py",
            "ops/release_artifact_manifest.py",
        ):
            with self.subTest(repository_path=repository_path):
                self.assertIn(repository_path, self.runbook)
        self.assertIn('< "$tools/deployment_preflight.py"', self.runbook)
        self.assertIn(
            'rsync -az "$tools/deployment_manifest.py"', self.runbook
        )
        self.assertNotIn("< ops/deployment_preflight.py", self.runbook)
        self.assertNotIn("rsync -az ops/deployment_manifest.py", self.runbook)
        self.assertNotIn("python3 ops/ci_metrics.py", self.runbook)


if __name__ == "__main__":
    unittest.main()
