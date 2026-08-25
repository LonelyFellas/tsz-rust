#!/usr/bin/env python3

import argparse
import fnmatch
import pathlib
import subprocess
import sys
from collections.abc import Callable, Iterable, Mapping, Sequence


MODULE_RULES: dict[str, tuple[str, ...]] = {
    "admin": ("admin_*",),
    "identity": (
        "account_deletion_handler",
        "auth_*",
        "otp_*",
        "refresh_tokens_schema",
        "register_session_transaction",
        "session_*",
        "student_profiles_schema",
        "teacher_profiles_schema",
        "user_*",
        "users_schema",
    ),
    "lexicon": (
        "content_completion_handler",
        "dictionary_schema",
        "lexicon_*",
    ),
    "platform": (
        "catalog_*",
        "health",
        "object_storage*",
        "redis_readiness",
        "speech_*",
    ),
}


class ClassificationError(ValueError):
    pass


def discover_targets(repository_root: pathlib.Path) -> tuple[str, ...]:
    tests_directory = repository_root / "tests"
    if not tests_directory.is_dir():
        raise ClassificationError(f"tests directory not found: {tests_directory}")
    return tuple(sorted(test_file.stem for test_file in tests_directory.glob("*.rs")))


def classify_targets(
    targets: Iterable[str],
    *,
    rules: Mapping[str, Sequence[str]] = MODULE_RULES,
) -> dict[str, tuple[str, ...]]:
    target_list = sorted(targets)
    if len(target_list) != len(set(target_list)):
        duplicates = sorted(
            target for target in set(target_list) if target_list.count(target) > 1
        )
        raise ClassificationError(f"duplicate targets: {', '.join(duplicates)}")

    grouped: dict[str, list[str]] = {module: [] for module in rules}
    unclassified: list[str] = []
    conflicts: list[str] = []

    for target in target_list:
        matched_modules = [
            module
            for module, patterns in rules.items()
            if any(fnmatch.fnmatchcase(target, pattern) for pattern in patterns)
        ]
        if not matched_modules:
            unclassified.append(target)
        elif len(matched_modules) > 1:
            conflicts.append(f"{target} -> {', '.join(sorted(matched_modules))}")
        else:
            grouped[matched_modules[0]].append(target)

    errors: list[str] = []
    if unclassified:
        errors.append(f"unclassified targets: {', '.join(unclassified)}")
    if conflicts:
        errors.append(f"targets match multiple modules: {'; '.join(conflicts)}")

    empty_modules = sorted(module for module, members in grouped.items() if not members)
    if empty_modules:
        errors.append(f"empty modules: {', '.join(empty_modules)}")
    if errors:
        raise ClassificationError("; ".join(errors))

    return {module: tuple(members) for module, members in grouped.items()}


def cargo_command(
    module: str, grouped_targets: Mapping[str, Sequence[str]]
) -> list[str]:
    if module not in grouped_targets:
        raise ClassificationError(f"unknown module: {module}")

    command = ["cargo", "test", "--locked", "--all-features"]
    for target in grouped_targets[module]:
        command.extend(("--test", target))
    return command


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Classify and run tsz-rust integration tests by module."
    )
    parser.add_argument(
        "--root",
        type=pathlib.Path,
        default=pathlib.Path(__file__).resolve().parent.parent,
        help="repository root (defaults to the parent of ops/)",
    )
    subparsers = parser.add_subparsers(dest="action", required=True)
    subparsers.add_parser("list", help="print the complete module inventory")
    run_parser = subparsers.add_parser("run", help="run one integration module")
    run_parser.add_argument("module")
    return parser


def main(
    argv: Sequence[str] | None = None,
    *,
    discovered_targets: Iterable[str] | None = None,
    rules: Mapping[str, Sequence[str]] = MODULE_RULES,
    runner: Callable[..., object] = subprocess.run,
) -> int:
    args = _parser().parse_args(argv)
    targets = (
        tuple(discovered_targets)
        if discovered_targets is not None
        else discover_targets(args.root)
    )
    grouped = classify_targets(targets, rules=rules)

    if args.action == "list":
        for module in sorted(grouped):
            print(f"{module}: {', '.join(grouped[module])}")
        return 0

    command = cargo_command(args.module, grouped)
    print(f"{args.module}: {', '.join(grouped[args.module])}", flush=True)
    runner(command, check=True)
    return 0


def _cli() -> int:
    try:
        return main()
    except ClassificationError as error:
        print(f"ci test module error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(_cli())
