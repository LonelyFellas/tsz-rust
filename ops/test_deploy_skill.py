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
        cls.claude_skill = (cls.root / ".claude/skills/deploy/SKILL.md").read_text(
            encoding="utf-8"
        )
        cls.agent_skill = (cls.root / ".agents/skills/deploy/SKILL.md").read_text(
            encoding="utf-8"
        )

    def test_skill_copies_share_one_artifact_contract(self) -> None:
        self.assertEqual(self.claude_skill, self.agent_skill)
        self.assertIn("tsz-rust-x86_64-linux-gnu-$deploy_sha", self.claude_skill)
        self.assertIn("attempt-$ci_run_attempt", self.claude_skill)
        self.assertIn("artifact_name\" =~ ^tsz-rust", self.claude_skill)
        self.assertNotIn("--jq --arg", self.claude_skill)
        self.assertIn('--jq ".artifacts[] | select(.name == \\"$artifact_name\\")', self.claude_skill)
        self.assertIn('"$tools/release_artifact_manifest.py" verify', self.claude_skill)
        self.assertIn("--expected-run-attempt", self.claude_skill)
        self.assertNotIn(
            "--name tsz-rust-x86_64-linux-gnu --dir", self.claude_skill
        )

    def test_skill_uses_machine_preflight_and_reports_deploy_timings(self) -> None:
        self.assertIn('< "$tools/deployment_preflight.py"', self.claude_skill)
        self.assertIn("--name deploy-artifact-download", self.claude_skill)
        self.assertIn("--name deploy-binary-rsync", self.claude_skill)
        self.assertIn("--name deploy-server-build", self.claude_skill)
        self.assertNotIn("cargo build --release", self.claude_skill)

    def test_artifact_is_verified_before_any_server_mutation(self) -> None:
        download = self.claude_skill.index("--name deploy-artifact-download")
        server_lock = self.claude_skill.index("server_lock_result=")
        backup = self.claude_skill.index("deploy_backup_dir=$(ssh")
        self.assertLess(download, server_lock)
        self.assertLess(server_lock, backup)

    def test_state_staging_and_server_mutations_are_session_owned(self) -> None:
        self.assertIn("~/.config/tsz-rust/deploy.lock/state.env", self.claude_skill)
        self.assertIn("~/.cache/tsz-rust-artifacts", self.claude_skill)
        self.assertIn("/opt/tsz-rust/deploy.lock/owner", self.claude_skill)
        self.assertIn("$deploy_session", self.claude_skill)
        self.assertNotIn("~/.config/tsz-rust/deploy-state.env", self.claude_skill)
        self.assertNotIn("~/.cache/tsz-rust-artifact\n", self.claude_skill)
        self.assertNotIn('rm -rf "$staging"', self.claude_skill)

    def test_every_bash_block_is_fail_fast_and_pipe_safe(self) -> None:
        bash_blocks = re.findall(
            r"(?ms)^\s*```bash\n(.*?)^\s*```$", self.claude_skill
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
            self.claude_skill.count('ssh tshb-test "set -eu; test'), 2
        )

    def test_remote_values_are_validated_or_shell_escaped_before_state_append(self) -> None:
        self.assertIn("manifest git_sha 非法", self.claude_skill)
        self.assertIn("deploy_backup_dir\" =~", self.claude_skill)
        self.assertIn("printf 'deployed_sha=%q", self.claude_skill)
        self.assertIn("printf 'deploy_backup_dir=%q", self.claude_skill)
        self.assertNotIn("deployed_sha=$deployed_sha\n", self.claude_skill)
        self.assertNotIn("deploy_backup_dir=$deploy_backup_dir\n", self.claude_skill)

    def test_owner_checks_share_the_remote_mutation_command(self) -> None:
        self.assertNotIn('ssh tshb-test "test', self.claude_skill)
        self.assertIn(
            'deploy_tool_incoming="/opt/tsz-deploy-tools/'
            'backend-deployment-manifest.incoming.$deploy_session"',
            self.claude_skill,
        )
        self.assertIn(
            'binary_incoming="/opt/tsz-rust/target/release/'
            'tsz-rust.incoming.$deploy_session"',
            self.claude_skill,
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
                position = self.claude_skill.index(mutation)
                command_start = self.claude_skill.rfind("ssh tshb-test", 0, position)
                owner_check = self.claude_skill.find("deploy.lock/owner", command_start, position)
                self.assertGreaterEqual(owner_check, command_start)

    def test_deploy_tools_are_extracted_from_the_verified_git_sha(self) -> None:
        self.assertIn(
            'git show "$deploy_sha:.claude/skills/deploy/scripts/require-green-main.sh"',
            self.claude_skill,
        )
        self.assertIn('git show "$deploy_sha:$tool_path"', self.claude_skill)
        for repository_path in (
            "ops/ci_fingerprint.py",
            "ops/ci_metrics.py",
            "ops/deployment_manifest.py",
            "ops/deployment_preflight.py",
            "ops/release_artifact_manifest.py",
        ):
            with self.subTest(repository_path=repository_path):
                self.assertIn(repository_path, self.claude_skill)
        self.assertIn('< "$tools/deployment_preflight.py"', self.claude_skill)
        self.assertIn(
            'rsync -az "$tools/deployment_manifest.py"', self.claude_skill
        )
        self.assertNotIn("< ops/deployment_preflight.py", self.claude_skill)
        self.assertNotIn("rsync -az ops/deployment_manifest.py", self.claude_skill)
        self.assertNotIn("python3 ops/ci_metrics.py", self.claude_skill)


if __name__ == "__main__":
    unittest.main()
