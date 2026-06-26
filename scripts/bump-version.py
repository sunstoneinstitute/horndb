#!/usr/bin/env python3
"""Bump the single workspace version in the root Cargo.toml.

Adapted from sunstoneinstitute/claude-plugins
(scripts/bump-plugin-version.py). HornDB carries one version under
``[workspace.package]`` that every member crate inherits via
``version.workspace = true``, so there is exactly one number to bump —
unlike the plugins repo, which bumps a per-plugin catalog.

Usage::

    bump-version.py --bump-type major|minor|patch [--cargo PATH]

Rewrites ``Cargo.toml`` in place and prints the new version to stdout (the
merge workflow captures it for the commit message and the git tag).
"""

import argparse
import re
import sys
from pathlib import Path

VERSION_RE = re.compile(r'^version\s*=\s*"([^"]+)"\s*$')


def bump_version(version: str, bump_type: str) -> str:
    """Increment a semantic ``X.Y.Z`` string."""
    parts = version.split(".")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        sys.exit(f"ERROR: not a 3-part numeric semver: {version!r}")
    major, minor, patch = (int(p) for p in parts)
    if bump_type == "major":
        return f"{major + 1}.0.0"
    if bump_type == "minor":
        return f"{major}.{minor + 1}.0"
    if bump_type == "patch":
        return f"{major}.{minor}.{patch + 1}"
    sys.exit(f"ERROR: unknown bump type: {bump_type!r}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bump-type", required=True, choices=["major", "minor", "patch"])
    ap.add_argument("--cargo", default="Cargo.toml", help="path to the root Cargo.toml")
    args = ap.parse_args()

    path = Path(args.cargo)
    lines = path.read_text().splitlines(keepends=True)

    # Walk the file tracking the current TOML table; only the `version` key
    # directly under [workspace.package] is the workspace version. Member
    # crates reference it as `version.workspace = true`, so they never carry a
    # literal `version = "..."` to be confused with.
    section = None
    for i, raw in enumerate(lines):
        stripped = raw.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped
            continue
        if section != "[workspace.package]":
            continue
        match = VERSION_RE.match(stripped)
        if match:
            new = bump_version(match.group(1), args.bump_type)
            lines[i] = f'version = "{new}"\n'
            path.write_text("".join(lines))
            print(new)
            return

    sys.exit(f"ERROR: no `version = \"...\"` under [workspace.package] in {path}")


if __name__ == "__main__":
    main()
