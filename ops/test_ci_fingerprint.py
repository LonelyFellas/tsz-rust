import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))

from ci_fingerprint import build_fingerprint  # noqa: E402


RUSTC_VERBOSE = """rustc 1.90.0 (1159e78c4 2025-09-14)
binary: rustc
commit-hash: 1159e78c4747b02ef996e55082b704c09b970588
commit-date: 2025-09-14
host: x86_64-unknown-linux-gnu
release: 1.90.0
LLVM version: 20.1.8
"""


class CiFingerprintTests(unittest.TestCase):
    def _repository(self, root: pathlib.Path) -> None:
        (root / ".cargo").mkdir()
        (root / ".sqlx").mkdir()
        (root / "Cargo.lock").write_text("lock-v1\n", encoding="utf-8")
        (root / "Cargo.toml").write_text("[package]\nname='demo'\n", encoding="utf-8")
        (root / "rust-toolchain.toml").write_text(
            '[toolchain]\nchannel = "1.90.0"\n', encoding="utf-8"
        )
        (root / ".cargo/config.toml").write_text(
            "[build]\nrustflags=[]\n", encoding="utf-8"
        )
        (root / "build.rs").write_text("fn main() {}\n", encoding="utf-8")
        (root / ".sqlx/query-a.json").write_text('{"query":"SELECT 1"}\n')

    def _fingerprint(self, root: pathlib.Path) -> dict:
        return build_fingerprint(
            repository_root=root,
            runner_os="Linux",
            runner_arch="X64",
            rustc_verbose=RUSTC_VERBOSE,
            cargo_version="cargo 1.90.0 (840b83a10 2025-07-30)",
            profile="test",
            features="all",
            sqlx_offline="true",
        )

    def test_ci01_key_contains_every_required_build_dimension(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._repository(root)
            result = self._fingerprint(root)

            self.assertEqual(result["runner"], {"os": "Linux", "arch": "X64"})
            self.assertEqual(result["rust"]["release"], "1.90.0")
            self.assertEqual(result["rust"]["host"], "x86_64-unknown-linux-gnu")
            self.assertEqual(result["build"]["profile"], "test")
            self.assertEqual(result["build"]["features"], "all")
            self.assertEqual(result["build"]["sqlx_offline"], "true")
            self.assertRegex(result["inputs"]["cargo_lock_sha256"], r"^[0-9a-f]{64}$")
            self.assertRegex(result["inputs"]["build_config_sha256"], r"^[0-9a-f]{64}$")
            self.assertRegex(result["inputs"]["sqlx_sha256"], r"^[0-9a-f]{64}$")
            self.assertIn("Linux-X64-rustc-1.90.0", result["cache_key"])
            self.assertIn("1159e78c4747b02ef996e55082b704c09b970588", result["cache_key"])
            self.assertIn("host-x86_64-unknown-linux-gnu", result["cache_key"])
            self.assertIn("profile-test", result["cache_key"])
            self.assertIn("features-all", result["cache_key"])
            self.assertIn("sqlx-true", result["cache_key"])

    def test_ci02_lock_config_and_sqlx_changes_invalidate_the_key(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._repository(root)
            original = self._fingerprint(root)

            cases = [
                (root / "Cargo.lock", "lock-v2\n"),
                (root / "build.rs", "fn main() { println!(\"changed\"); }\n"),
                (root / ".sqlx/query-a.json", '{"query":"SELECT 2"}\n'),
            ]
            for path, content in cases:
                with self.subTest(path=path.name):
                    before = self._fingerprint(root)
                    path.write_text(content, encoding="utf-8")
                    after = self._fingerprint(root)
                    self.assertNotEqual(before["cache_key"], after["cache_key"])

            self.assertNotEqual(original["cache_key"], self._fingerprint(root)["cache_key"])

    def test_ci03_tampered_cache_key_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self._repository(root)
            result = self._fingerprint(root)
            result["cache_key"] = "v3-tampered"
            from ci_fingerprint import FingerprintError, validate_fingerprint

            with self.assertRaises(FingerprintError):
                validate_fingerprint(result)


if __name__ == "__main__":
    unittest.main()
