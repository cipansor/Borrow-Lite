#!/usr/bin/env python3
"""Resolve a release tag to one crate and one version.

Releases here are per crate, because the two crates version independently: a tag
is `<crate>-v<version>` and publishes exactly that crate. There is deliberately
no bare `v<version>` tag - one tag cannot describe both crates, which may sit at
different versions, and treating it as "release everything" is how a crate with
no changes ends up with a version bump it never earned.

    python3 scripts/release.py --check borrow_lite-v0.1.1
    python3 scripts/release.py --tag borrow_lite-v0.1.1 --print crate
    python3 scripts/release.py --tag borrow_lite-v0.1.1 --print version
    python3 scripts/release.py --notes borrow_lite-v0.1.1 --out notes.md

The manifest is read with `tomllib`, not `cargo metadata`, so a wrong tag is
rejected in milliseconds, before a toolchain is installed.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# `<crate>-v<version>`, e.g. `borrow_lite-v0.1.1`. The crate part allows the
# underscore/dash/hyphen shapes a Cargo package name may take; the version starts
# at a digit so `borrow_lite-vnext` is not mistaken for one.
TAG_PATTERN = re.compile(r"^(?P<crate>[A-Za-z0-9_-]+)-v(?P<version>[0-9][^/]*)$")

# The root manifest is both the `borrow_lite` package and the workspace root.
ROOT_MANIFEST = "Cargo.toml"


class ReleaseError(Exception):
    """A request that cannot be carried out: a bad tag, or an unknown crate."""


def crates_from_tag(tag: str) -> tuple[str, str]:
    """Return (crate, version) named by a `<crate>-v<version>` release tag.

    A bare `v<version>` tag is refused rather than interpreted. It would be
    ambiguous - the crates version independently - and guessing "release
    everything" is exactly the behaviour this scheme exists to prevent.
    """
    if tag.startswith("v"):
        raise ReleaseError(
            f"`{tag}` is a bare version tag, which this repository does not use. "
            "The crates version independently, so a release tag names the crate: "
            "`<crate>-v<version>`, for example `borrow_lite-v0.1.1`."
        )
    match = TAG_PATTERN.match(tag)
    if not match:
        raise ReleaseError(
            f"`{tag}` is not a release tag. Name the crate and the version: "
            "`<crate>-v<version>`, for example `borrow_lite-v0.1.1`."
        )
    return match.group("crate"), match.group("version")


def load_manifests() -> tuple[dict[str, dict], dict[str, Path]]:
    """Return (crate name -> manifest, crate name -> directory).

    The root `Cargo.toml` is the `borrow_lite` package; `derive/` holds
    `borrow_lite_derive`. A workspace member whose directory does not exist is
    reported rather than skipped, so a renamed directory cannot silently shrink
    the set of releasable crates.
    """
    root = tomllib.loads((ROOT / ROOT_MANIFEST).read_text())
    manifests: dict[str, dict] = {root["package"]["name"]: root}
    dirs: dict[str, Path] = {root["package"]["name"]: ROOT}

    for member in root.get("workspace", {}).get("members", []):
        path = ROOT / member / "Cargo.toml"
        if not path.is_file():
            raise ReleaseError(f"workspace member `{member}` has no Cargo.toml")
        parsed = tomllib.loads(path.read_text())
        name = parsed["package"]["name"]
        manifests[name] = parsed
        dirs[name] = ROOT / member
    return manifests, dirs


def version_of(manifest: dict) -> str:
    """The version a manifest declares.

    Both crates state their own `version`. The `version.workspace = true` form is
    resolved too, so a manifest that inherits a shared workspace version keeps
    working.
    """
    field = manifest["package"]["version"]
    if isinstance(field, str):
        return field
    workspace = tomllib.loads((ROOT / ROOT_MANIFEST).read_text())
    return workspace["workspace"]["package"]["version"]


def check_tag(
    tag: str, manifests: dict[str, dict], dirs: dict[str, Path]
) -> tuple[str, str]:
    """Validate a release tag against the manifests and return (crate, version).

    Only the tag and the named crate's own manifest are checked here. Whether the
    crate's *dependencies* are satisfiable is left to `cargo publish --dry-run`,
    which applies Cargo's real semver rules - a patch release of
    `borrow_lite_derive` is legitimately inside `borrow_lite`'s `^0.1.0`
    requirement, and a hand-rolled check here would wrongly refuse it.
    """
    crate, tag_version = crates_from_tag(tag)
    if crate not in manifests:
        known = ", ".join(sorted(manifests))
        raise ReleaseError(
            f"`{tag}` names `{crate}`, which is not a crate here. Known: {known}"
        )
    manifest_version = version_of(manifests[crate])
    if manifest_version != tag_version:
        raise ReleaseError(
            f"`{tag}` says {crate} is {tag_version}, but its manifest says "
            f"{manifest_version}. Update the manifest, or move the tag."
        )
    return crate, tag_version


def pathspec_for(crate: str, dirs: dict[str, Path]) -> list[str]:
    """The git pathspec covering exactly one crate's files.

    `borrow_lite` lives at the repository root, so its path is everything except
    the sibling's directory - a plain `.` would pull the derive crate's commits
    into the main crate's notes.
    """
    directory = dirs[crate]
    if directory == ROOT:
        others = [str(d.relative_to(ROOT)) for name, d in dirs.items() if name != crate]
        return [".", *(f":(exclude){o}" for o in others)]
    return [str(directory.relative_to(ROOT))]


def pick_tag(tags: list[str], exclude: str) -> str | None:
    """The first tag in `tags` that is not `exclude`.

    `tags` is expected newest-first, which is the order `git tag --sort=-v:refname`
    returns and the order the caller wants.
    """
    return next((tag for tag in tags if tag != exclude), None)


def _highest_tag(pattern: str, exclude: str) -> str | None:
    """The most recent tag matching `pattern`, ignoring `exclude`."""
    tags = subprocess.run(
        ["git", "tag", "-l", pattern, "--sort=-v:refname"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    return pick_tag(tags, exclude)


def previous_tag(crate: str, current: str) -> str | None:
    """The tag a release of `crate` should be compared against.

    Prefers the crate's own last tag, so notes cover only what changed since that
    crate last shipped. Falls back to a bare `v<version>` tag: 0.1.0 was released
    for both crates under one such tag, and that is still a correct baseline for
    the first per-crate release after it.
    """
    return _highest_tag(f"{crate}-v*", current) or _highest_tag("v*", current)


def _ref_exists(ref: str) -> bool:
    """Whether a git ref resolves in this checkout."""
    result = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return result.returncode == 0


def release_notes(tag: str, dirs: dict[str, Path], crate: str) -> str:
    """Commit subjects that touched this crate since its previous release.

    The workflow runs on the tag push, so the tag resolves. A local run may name
    a tag that is not created yet; that falls back to `HEAD` rather than failing,
    so the notes can be previewed before the tag exists.
    """
    previous = previous_tag(crate, tag)
    tip = tag if _ref_exists(tag) else "HEAD"
    span = f"{previous}..{tip}" if previous else tip
    result = subprocess.run(
        [
            "git",
            "log",
            "--no-merges",
            "--format=- %s (%h)",
            span,
            "--",
            *pathspec_for(crate, dirs),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    lines = result.stdout.strip()
    header = f"Changes in `{crate}`"
    if previous:
        header += f" since `{previous}`"
    body = lines if lines else "- No changes recorded for this crate."
    return f"{header}\n\n{body}\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", help="the release tag, e.g. borrow_lite-v0.1.1")
    parser.add_argument(
        "--check",
        metavar="TAG",
        help="validate a release tag and exit, without touching git history",
    )
    parser.add_argument(
        "--print",
        dest="field",
        choices=["crate", "version"],
        help="print one field of the resolved tag, for use in a workflow",
    )
    parser.add_argument("--notes", metavar="TAG", help="write release notes for TAG")
    parser.add_argument("--out", help="file to write --notes to")
    args = parser.parse_args(argv)

    try:
        manifests, dirs = load_manifests()
        if args.check:
            crate, version = check_tag(args.check, manifests, dirs)
            print(f"`{args.check}` names {crate} {version}")
            return 0
        if args.notes:
            crate, _ = check_tag(args.notes, manifests, dirs)
            notes = release_notes(args.notes, dirs, crate)
            if args.out:
                Path(args.out).write_text(notes)
                print(f"wrote {args.out}")
            else:
                print(notes)
            return 0
        if not args.tag:
            parser.error("one of --tag, --check or --notes is required")
        crate, version = check_tag(args.tag, manifests, dirs)
        if args.field == "crate":
            print(crate)
        elif args.field == "version":
            print(version)
        else:
            print(f"{crate} {version}")
        return 0
    except ReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
