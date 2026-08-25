import contextlib
import io
import pathlib
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).parent))

from ci_test_modules import (  # noqa: E402
    ClassificationError,
    cargo_command,
    classify_targets,
    discover_targets,
    main,
)


EXPECTED_TARGETS = {
    "admin": {
        "admin_accounts_create_handler",
        "admin_accounts_list_handler",
        "admin_accounts_repository",
        "admin_accounts_reset_password_handler",
        "admin_accounts_status_handler",
        "admin_login_code_handler",
        "admin_login_handler",
        "admin_logout_all_handler",
        "admin_preferences_handler",
        "admin_profile_handler",
        "admin_refresh_handler",
        "admin_repository",
        "admin_schema",
        "admin_seed_service",
        "admin_session_repository",
        "admin_session_reuse_detection",
        "admin_session_service",
        "admin_user_detail_handler",
        "admin_user_list_handler",
    },
    "identity": {
        "account_deletion_handler",
        "auth_extract",
        "auth_login_handler",
        "auth_login_otp_handler",
        "auth_me",
        "auth_refresh_handler",
        "otp_send_handler",
        "otp_service",
        "otp_store",
        "refresh_tokens_schema",
        "register_session_transaction",
        "session_repository",
        "session_reuse_detection",
        "session_service",
        "student_profiles_schema",
        "teacher_profiles_schema",
        "user_authenticate",
        "user_register_handler",
        "user_repository",
        "user_roles_schema",
        "user_service",
        "users_schema",
    },
    "lexicon": {
        "content_completion_handler",
        "dictionary_schema",
        "lexicon_handler",
        "lexicon_schema",
        "lexicon_surface_projection",
        "lexicon_v3_lifecycle",
        "lexicon_v3_migration",
        "lexicon_v3_relation_consumers",
        "lexicon_v3_storage_schema",
    },
    "platform": {
        "catalog_handler",
        "catalog_schema",
        "health",
        "object_storage",
        "object_storage_oss_smoke",
        "redis_readiness",
        "speech_preview_cleanup",
        "speech_preview_faults",
        "speech_preview_handler",
        "speech_preview_schema",
        "speech_preview_service",
        "speech_voice_catalog_seed",
    },
}


class CiTestModulesTests(unittest.TestCase):
    def test_ci01_current_inventory_is_partitioned_exactly_once(self) -> None:
        repository_root = pathlib.Path(__file__).resolve().parent.parent

        actual = classify_targets(discover_targets(repository_root))

        self.assertEqual(
            {module: set(targets) for module, targets in actual.items()},
            EXPECTED_TARGETS,
        )
        flattened = [target for targets in actual.values() for target in targets]
        self.assertEqual(len(flattened), len(set(flattened)))
        self.assertEqual(len(flattened), 62)

    def test_ci02_unknown_target_fails_closed(self) -> None:
        with self.assertRaisesRegex(
            ClassificationError, "unclassified.*mystery_handler"
        ):
            classify_targets(
                [*self._one_target_per_module(), "mystery_handler"]
            )

    def test_ci03_overlapping_rules_report_every_matching_module(self) -> None:
        rules = {
            "first": ("shared_*",),
            "second": ("*_target",),
        }

        with self.assertRaisesRegex(
            ClassificationError, "shared_target.*first.*second"
        ):
            classify_targets(["shared_target"], rules=rules)

    def test_ci04_empty_module_fails_before_execution(self) -> None:
        rules = {
            "first": ("first_*",),
            "second": ("second_*",),
        }

        with self.assertRaisesRegex(ClassificationError, "empty.*second"):
            classify_targets(["first_target"], rules=rules)

    def test_ci05_cargo_command_is_locked_feature_complete_and_unique(self) -> None:
        grouped = classify_targets(
            self._one_target_per_module(), rules=self._synthetic_rules()
        )

        command = cargo_command("identity", grouped)

        self.assertEqual(
            command[:5],
            ["cargo", "test", "--locked", "--all-features", "--test"],
        )
        self.assertEqual(command.count("identity_target"), 1)
        self.assertEqual(command.count("--test"), 1)

    def test_ci06_list_mode_is_stable_and_never_starts_cargo(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests = root / "tests"
            tests.mkdir()
            for target in self._one_target_per_module():
                (tests / f"{target}.rs").touch()
            runner = mock.Mock()
            output = io.StringIO()

            with contextlib.redirect_stdout(output):
                exit_code = main(
                    ["--root", str(root), "list"],
                    rules=self._synthetic_rules(),
                    runner=runner,
                )

            self.assertEqual(exit_code, 0)
            runner.assert_not_called()
            self.assertEqual(
                output.getvalue().splitlines(),
                [
                    "admin: admin_target",
                    "identity: identity_target",
                    "lexicon: lexicon_target",
                    "platform: platform_target",
                ],
            )

    def test_ci07_run_mode_rejects_an_unknown_module(self) -> None:
        runner = mock.Mock()

        with self.assertRaisesRegex(ClassificationError, "unknown module.*billing"):
            main(
                ["run", "billing"],
                discovered_targets=self._one_target_per_module(),
                rules=self._synthetic_rules(),
                runner=runner,
            )

        runner.assert_not_called()

    def test_ci08_run_mode_invokes_cargo_once(self) -> None:
        runner = mock.Mock()
        output = io.StringIO()

        with contextlib.redirect_stdout(output):
            exit_code = main(
                ["run", "lexicon"],
                discovered_targets=self._one_target_per_module(),
                rules=self._synthetic_rules(),
                runner=runner,
            )

        self.assertEqual(exit_code, 0)
        self.assertEqual(output.getvalue(), "lexicon: lexicon_target\n")
        runner.assert_called_once_with(
            [
                "cargo",
                "test",
                "--locked",
                "--all-features",
                "--test",
                "lexicon_target",
            ],
            check=True,
        )

    @staticmethod
    def _one_target_per_module() -> list[str]:
        return [
            "admin_target",
            "identity_target",
            "lexicon_target",
            "platform_target",
        ]

    @staticmethod
    def _synthetic_rules() -> dict[str, tuple[str, ...]]:
        return {
            "admin": ("admin_*",),
            "identity": ("identity_*",),
            "lexicon": ("lexicon_*",),
            "platform": ("platform_*",),
        }


if __name__ == "__main__":
    unittest.main()
