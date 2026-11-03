#!/usr/bin/env python3
"""Conservative PR documentation classification; uncertainty runs full CI."""
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess


def docs_only(paths):
    return bool(paths) and all(
        len((parts := PurePosixPath(path).parts)) >= 3
        and parts[0] == "docs" and parts[1] in {"ci", "features"}
        and ".." not in parts and PurePosixPath(path).suffix == ".md"
        for path in paths
    )


def classify(event_name, event):
    if event_name != "pull_request":
        return False
    pr = event["pull_request"]
    base, head = pr["base"]["sha"], pr["head"]["sha"]
    if not all(isinstance(sha, str) and re.fullmatch(r"[0-9a-f]{40}", sha) for sha in (base, head)):
        raise ValueError("invalid revision")
    changed = subprocess.check_output(["git", "diff", "--name-only", "--no-renames", "-z", f"{base}...{head}"], text=True)
    return docs_only(changed.removesuffix("\0").split("\0") if changed else [])


def main():
    try:
        only_docs = classify(os.environ.get("GITHUB_EVENT_NAME"), json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text()))
    except (OSError, KeyError, ValueError, subprocess.CalledProcessError):
        only_docs = False
    value = str(only_docs).lower()
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"docs_only={value}\n")
    print("Documentation-only PR: " + value + "; main and unknown changes always run full CI.")


if __name__ == "__main__":
    main()
