#!/usr/bin/env python3
"""Manual release automation for HornDB.

Reads a changelog entry from stdin, bumps the single workspace version in the
root ``Cargo.toml`` (under ``[workspace.package]`` — every member crate inherits
it via ``version.workspace = true``), prepends the entry to ``CHANGELOG.md``,
syncs ``Cargo.lock``, commits, and tags ``vX.Y.Z``.

This is the *manual* release path, driven by the ``/release`` command. It is not
the CI label-driven bump (``bump-version-on-merge.yml``); if you release with
this script, do not also apply a ``bump-*`` label to the PR.

Usage::

    echo "changelog text" | python3 .claude/scripts/release.py minor
    echo "changelog text" | python3 .claude/scripts/release.py 1.2.0
    echo "changelog text" | python3 .claude/scripts/release.py v1.2.0
    echo "changelog text" | python3 .claude/scripts/release.py --bump minor
    echo "changelog text" | python3 .claude/scripts/release.py --version 1.2.0

The positional argument accepts ``major``/``minor``/``patch`` or an explicit
version (``1.2.0`` or ``v1.2.0`` — a leading ``v`` is stripped).
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from datetime import date
from pathlib import Path

# .claude/scripts/release.py -> repo root is three parents up.
ROOT = Path(__file__).resolve().parents[2]
CARGO = ROOT / "Cargo.toml"
CHANGELOG = ROOT / "CHANGELOG.md"

VERSION_RE = re.compile(r'^version\s*=\s*"([^"]+)"\s*$')


def _find_workspace_version(lines: list[str]) -> int:
    """Return the index of the ``version = "..."`` line under
    ``[workspace.package]``. Section-aware so it never matches a member crate's
    ``version.workspace = true`` or an unrelated ``version = "..."`` dependency
    pin."""
    section = None
    for i, raw in enumerate(lines):
        stripped = raw.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped
            continue
        if section != "[workspace.package]":
            continue
        if VERSION_RE.match(stripped):
            return i
    sys.exit('Could not find version = "..." under [workspace.package] in Cargo.toml')


def current_version() -> str:
    lines = CARGO.read_text().splitlines(keepends=True)
    i = _find_workspace_version(lines)
    return VERSION_RE.match(lines[i].strip()).group(1)


def normalize_version(version: str) -> str:
    """Strip a leading ``v`` and validate a 3-part numeric semver."""
    v = version[1:] if version.startswith("v") else version
    parts = v.split(".")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        sys.exit(f"Expected semver x.y.z, got {version}")
    return v


def bump_version(version: str, bump: str) -> str:
    parts = version.split(".")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        sys.exit(f"Expected semver x.y.z, got {version}")
    major, minor, patch = (int(p) for p in parts)
    if bump == "major":
        return f"{major + 1}.0.0"
    elif bump == "minor":
        return f"{major}.{minor + 1}.0"
    else:
        return f"{major}.{minor}.{patch + 1}"


def update_cargo(new: str) -> None:
    lines = CARGO.read_text().splitlines(keepends=True)
    i = _find_workspace_version(lines)
    lines[i] = f'version = "{new}"\n'
    CARGO.write_text("".join(lines))


def update_changelog(new_version: str, entry: str) -> None:
    today = date.today().isoformat()
    header = f"## [{new_version}] - {today}"

    if CHANGELOG.exists():
        text = CHANGELOG.read_text()
        # Insert after ## [Unreleased] if present, otherwise after the title.
        # Also clear any content under [Unreleased] to avoid duplication.
        unreleased_with_content = r"(## \[Unreleased\])\n(?:.*?\n)*?(?=## \[)"
        unreleased_bare = r"(## \[Unreleased\]\n)"
        if re.search(unreleased_with_content, text):
            text = re.sub(
                unreleased_with_content,
                f"\\1\n\n{header}\n\n{entry.strip()}\n\n",
                text,
                count=1,
            )
        elif re.search(unreleased_bare, text):
            text = re.sub(
                unreleased_bare,
                f"\\1\n{header}\n\n{entry.strip()}\n\n",
                text,
                count=1,
            )
        else:
            # Insert after the first heading
            text = re.sub(
                r"(# Changelog\n)",
                f"\\1\n{header}\n\n{entry.strip()}\n\n",
                text,
                count=1,
            )
    else:
        text = (
            "# Changelog\n\n"
            "All notable changes to this project will be documented in this file.\n\n"
            "The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),\n"
            "and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).\n\n"
            "## [Unreleased]\n\n"
            f"{header}\n\n{entry.strip()}\n"
        )
    CHANGELOG.write_text(text)


def sync_cargo_lock() -> None:
    # Keep Cargo.lock's member entries in lockstep with the bumped manifest.
    # Members are local, so --offline needs no registry access and does not
    # touch external dependency pins.
    result = subprocess.run(
        ["cargo", "update", "--workspace", "--offline"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(f"Error: cargo update --workspace failed\n{result.stderr}", file=sys.stderr)
        sys.exit(1)


def git_commit_and_tag(version: str) -> None:
    tag = f"v{version}"
    subprocess.run(
        ["git", "add", "Cargo.toml", "Cargo.lock", "CHANGELOG.md"],
        cwd=ROOT,
        check=True,
    )
    subprocess.run(
        ["git", "commit", "-m", f"release: {tag}"],
        cwd=ROOT,
        check=True,
    )
    subprocess.run(
        ["git", "tag", "-a", tag, "-m", f"Release {tag}"],
        cwd=ROOT,
        check=True,
    )
    print(f"Created commit and tag {tag}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Release HornDB")
    parser.add_argument(
        "spec",
        nargs="?",
        help="major|minor|patch, or an explicit version (1.2.0 / v1.2.0)",
    )
    group = parser.add_mutually_exclusive_group()
    group.add_argument(
        "--bump",
        choices=["major", "minor", "patch"],
        help="Version bump type (relative to the current version)",
    )
    group.add_argument(
        "--version",
        help="Explicit new version X.Y.Z (used verbatim instead of bumping)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Show what would happen without making changes",
    )
    args = parser.parse_args()

    # Resolve the version source: exactly one of the positional spec, --bump, or
    # --version. A bare positional is either a bump level or an explicit version.
    bump = args.bump
    version = args.version
    if args.spec is not None:
        if bump is not None or version is not None:
            sys.exit("Provide a version/bump either positionally or via a flag, not both")
        if args.spec in ("major", "minor", "patch"):
            bump = args.spec
        else:
            version = args.spec
    if bump is None and version is None:
        sys.exit("Provide a bump level (major|minor|patch) or an explicit version")

    changelog_entry = sys.stdin.read().strip()
    if not changelog_entry:
        sys.exit("No changelog entry provided on stdin")

    old = current_version()
    if version is not None:
        new = normalize_version(version)
        if new == old:
            sys.exit(f"New version {new} is the same as the current version")
    else:
        new = bump_version(old, bump)

    print(f"Version: {old} -> {new}")
    print(f"Changelog:\n{changelog_entry}\n")

    if args.dry_run:
        print("(dry run — no changes made)")
        return

    update_cargo(new)
    sync_cargo_lock()
    update_changelog(new, changelog_entry)
    git_commit_and_tag(new)


if __name__ == "__main__":
    main()
