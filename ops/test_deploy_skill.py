import pathlib
import re
import shlex
import subprocess
import tempfile
import textwrap
import unittest

# runbook 里 bash 代码块的唯一解析口径：fail-fast 检查与推送 rsync 检查共用。
BASH_FENCE = re.compile(r"^[ \t]*```(?:bash|sh|shell)[ \t]*\n(.*?)^[ \t]*```[ \t]*$", re.M | re.S)


def remote_rsync_commands(markdown: str) -> list[str]:
    """markdown 的 bash 代码块里推到 tshb-test 的 rsync 命令（`\\` 续行已拼成一条）。"""
    commands = []

    def collect(logical: str) -> None:
        if re.search(r"\brsync\b", logical) and "tshb-test:" in logical:
            commands.append(logical)

    for block in BASH_FENCE.findall(markdown):
        logical = ""
        for line in block.splitlines():
            stripped = line.strip()
            if not logical and (not stripped or stripped.startswith("#")):
                continue
            if stripped.endswith("\\"):
                logical += stripped[:-1] + " "
                continue
            collect(logical + stripped)
            logical = ""
        if logical:
            collect(logical)
    return commands


SHELL_PUNCTUATION = "();<>|&"
# rsync 接受无歧义的长选项前缀（如 --arch、--no-own）：(完整名, 最短可认前缀长度, 影响的开关, 打开或关闭)。
RSYNC_OWNER_LONG_OPTIONS = (
    ("--archive", 4, ("owner", "group"), True),
    ("--owner", 4, ("owner",), True),
    ("--group", 4, ("group",), True),
    ("--no-owner", 6, ("owner",), False),
    ("--no-group", 6, ("group",), False),
)


def strip_shell_comment(command: str) -> str:
    """按 bash 规则去注释：只有不在引号内、且位于词首的 # 才开始注释（`${x#y}`、`a#b` 不算）。"""
    quote = None
    escaped = False
    for index, char in enumerate(command):
        if escaped:
            escaped = False
        elif char == "\\" and quote != "'":
            escaped = True
        elif quote:
            if char == quote:
                quote = None
        elif char in "'\"":
            quote = char
        elif char == "#" and (index == 0 or command[index - 1].isspace()):
            return command[:index]
    return command


def rsync_keeps_local_owner(command: str) -> bool:
    """按 rsync「后写覆盖先写」模拟属主/属组开关，任一推到 tshb-test 的 rsync 最终仍保留即为 True。

    推到 tshb-test 却认不出 rsync 词元的调用（如 `"$(command -v rsync)"`）一律按违规处理。
    """
    lexer = shlex.shlex(strip_shell_comment(command), posix=True, punctuation_chars=True)
    lexer.whitespace_split = True
    lexer.commenters = ""
    segments, segment = [], []
    for token in lexer:
        is_operator = (
            all(char in SHELL_PUNCTUATION for char in token)
            and any(char in ";|&" for char in token)
            and not any(char in "<>" for char in token)
        )
        if is_operator:
            segments.append(segment)
            segment = []
        else:
            segment.append(token)
    segments.append(segment)
    for segment in segments:
        if not any("tshb-test:" in token for token in segment):
            continue
        names = [token.rsplit("/", 1)[-1] for token in segment]
        if "rsync" not in names:
            if any(re.search(r"\brsync\b", token) for token in segment):
                return True
            continue
        switches = {"owner": False, "group": False}
        for token in segment[names.index("rsync") + 1 :]:
            option = token.split("=", 1)[0]
            if option.startswith("--"):
                for name, shortest, affected, enabled in RSYNC_OWNER_LONG_OPTIONS:
                    if len(option) >= shortest and name.startswith(option):
                        for switch in affected:
                            switches[switch] = enabled
            elif re.fullmatch(r"-[A-Za-z]+", token):
                if "a" in token or "o" in token:
                    switches["owner"] = True
                if "a" in token or "g" in token:
                    switches["group"] = True
        if switches["owner"] or switches["group"]:
            return True
    return False


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
        self.assertIn("rollback_target_version", self.runbook)
        self.assertIn("rollback_expected_version", self.runbook)
        self.assertIn("--name deploy-artifact-download", self.runbook)
        self.assertIn("--name deploy-binary-rsync", self.runbook)
        self.assertIn("--name deploy-server-build", self.runbook)
        self.assertNotIn("cargo build --release", self.runbook)

    def test_database_rollback_precedes_binary_restore(self) -> None:
        stop = self.runbook.index("systemctl stop tsz-rust", self.runbook.index("## 6. 回退"))
        undo = self.runbook.index("deploy-undo-migrations", stop)
        restore = self.runbook.index("backend-deployment-manifest.py restore", undo)
        self.assertLess(stop, undo)
        self.assertLess(undo, restore)
        self.assertIn("database migration version is outside", self.runbook)

    def test_candidate_is_isolated_until_manifest_is_verified(self) -> None:
        candidate = self.runbook.index("BIND_IP=127.0.0.1 DEPLOYMENT_SMOKE_ONLY=true")
        manifest = self.runbook.index("backend-deployment-manifest.py create", candidate)
        formal_start = self.runbook.index("systemctl start tsz-rust", manifest)
        self.assertLess(candidate, manifest)
        self.assertLess(manifest, formal_start)
        self.assertIn("systemctl stop tsz-rust", self.runbook[:candidate])

    def test_deploy_without_new_migrations_keeps_a_binary_only_rollback(self) -> None:
        self.assertNotIn('test -n "$rollback_up_versions"', self.runbook)
        self.assertIn(
            "rollback_expected_version=${rollback_expected_version:-$rollback_target_version}",
            self.runbook,
        )
        self.assertIn(
            'test "$rollback_expected_version" -ge "$rollback_target_version"',
            self.runbook,
        )

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
        bash_blocks = BASH_FENCE.findall(self.runbook)
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
            'rsync -az --no-o --no-g "$tools/deployment_manifest.py"', self.runbook
        )
        self.assertIn(
            'rsync -az --no-o --no-g "$staging/tsz-rust"', self.runbook
        )
        self.assertNotIn("< ops/deployment_preflight.py", self.runbook)
        self.assertNotIn("rsync -az ops/deployment_manifest.py", self.runbook)
        self.assertNotIn("python3 ops/ci_metrics.py", self.runbook)

    def test_remote_rsync_commands_drop_local_owner(self) -> None:
        # 推到服务器的 rsync 不得保留本机属主：服务器上没有对应 uid，mv 到位后会留给 root 执行的文件。
        commands = remote_rsync_commands(self.runbook)
        self.assertGreaterEqual(len(commands), 2)
        for command in commands:
            with self.subTest(command=command):
                self.assertFalse(rsync_keeps_local_owner(command))

    def test_remote_rsync_scan_covers_new_forms(self) -> None:
        sample = textwrap.dedent(
            """
            正文提到 rsync 到 tshb-test: 不算命令。

            ```bash
            # rsync -az "$a" "tshb-test:/comment"
            rsync -avz "$a" "tshb-test:/plain"
            rsync -a $a tshb-test:/unquoted
            python3 "$tools/ci_metrics.py" run -- \\
              rsync -az \\
              "$a" "tshb-test:/continued"
            rsync --no-o --no-g -az "$a" "tshb-test:/flags-first"
            rsync -az "$a" "tshb-test:/comment-flags"  # --no-o --no-g
            rsync -az --no-o --no-g "$a" "tshb-test:/ok" && rsync -az "$b" "tshb-test:/chained"
            rsync -az "$a" "tshb-test:/and-first" && rsync --no-o --no-g "$b" "tshb-test:/ok-and-second"
            rsync -avz --no-owner --no-group -e "ssh -p 22" "$a" "tshb-test:/ok-long"
            rsync -az --no-o --no-g "$a" "tshb-test:/ok"
            rsync -az "$a" "$staging/local-only"
            rsync --no-o --no-g --arch "$a" "tshb-test:/abbrev-archive"
            rsync -az --no-own --no-gro "$a" "tshb-test:/ok-abbrev"
            "$(command -v rsync)" -az "$a" "tshb-test:/indirect"
            rsync -az "$a" "tshb-test:/glued";(rsync --no-o --no-g "$b" "tshb-test:/ok-glued-second")
            rsync -az "$a" "tshb-test:/pipe-amp"|&rsync --no-o --no-g "$b" "tshb-test:/ok-pipe-second"
            rsync -az ${opts#-} "$a" "tshb-test:/midword-hash"
            rsync -az --no-o --no-g "$a#b" "tshb-test:/ok-hash-in-quotes"
            rsync -az "$a" \\
              "tshb-test:/tail" \\
            ```
            """
        )
        offending = [
            # 每条命令取第一个非 ok- 目标作标识：违规写法都不以 ok- 命名，合规写法都以 ok- 命名。
            next(
                (
                    path
                    for path in re.findall(r"tshb-test:/[\w-]+", command)
                    if not path.startswith("tshb-test:/ok")
                ),
                command,
            )
            for command in remote_rsync_commands(sample)
            if rsync_keeps_local_owner(command)
        ]
        self.assertEqual(
            offending,
            [
                "tshb-test:/plain",
                "tshb-test:/unquoted",
                "tshb-test:/continued",
                "tshb-test:/flags-first",
                "tshb-test:/comment-flags",
                "tshb-test:/chained",
                "tshb-test:/and-first",
                "tshb-test:/abbrev-archive",
                "tshb-test:/indirect",
                "tshb-test:/glued",
                "tshb-test:/pipe-amp",
                "tshb-test:/midword-hash",
                "tshb-test:/tail",
            ],
        )


if __name__ == "__main__":
    unittest.main()
