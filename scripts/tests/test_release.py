"""Tests for the release tag resolver.

The rules covered here are the ones that fail quietly: a tag that names the wrong
crate, or a tag whose version disagrees with the manifest. Neither breaks a
build, so nothing else in CI would catch them - the mistake surfaces at the
upload, which cannot be undone.
"""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(SCRIPTS))

spec = importlib.util.spec_from_file_location("release", SCRIPTS / "release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class CratesFromTag(unittest.TestCase):
    """The tag is what a reader trusts, so its shape is enforced."""

    def test_names_a_crate_and_version(self):
        self.assertEqual(
            release.crates_from_tag("borrow_lite-v0.1.1"), ("borrow_lite", "0.1.1")
        )

    def test_names_the_proc_macro_crate(self):
        self.assertEqual(
            release.crates_from_tag("borrow_lite_derive-v0.2.0"),
            ("borrow_lite_derive", "0.2.0"),
        )

    def test_refuses_a_bare_version_tag(self):
        # The crates version independently, so a bare tag cannot say which crate
        # it meant. Interpreting it as "release everything" is the behaviour the
        # per-crate scheme exists to prevent.
        with self.assertRaises(release.ReleaseError) as caught:
            release.crates_from_tag("v0.1.1")
        self.assertIn("bare version tag", str(caught.exception))

    def test_refuses_a_tag_without_a_version(self):
        with self.assertRaises(release.ReleaseError):
            release.crates_from_tag("borrow_lite-vnext")

    def test_refuses_an_empty_version(self):
        with self.assertRaises(release.ReleaseError):
            release.crates_from_tag("borrow_lite-v")


class CheckTag(unittest.TestCase):
    """A tag has to agree with the manifest it names."""

    def setUp(self):
        self.manifests, self.dirs = release.load_manifests()

    def test_accepts_the_current_versions(self):
        for name, manifest in self.manifests.items():
            version = release.version_of(manifest)
            crate, resolved = release.check_tag(
                f"{name}-v{version}", self.manifests, self.dirs
            )
            self.assertEqual((crate, resolved), (name, version))

    def test_refuses_a_version_the_manifest_does_not_have(self):
        with self.assertRaises(release.ReleaseError) as caught:
            release.check_tag("borrow_lite-v9.9.9", self.manifests, self.dirs)
        self.assertIn("manifest says", str(caught.exception))

    def test_refuses_an_unknown_crate(self):
        with self.assertRaises(release.ReleaseError) as caught:
            release.check_tag("nope-v0.1.0", self.manifests, self.dirs)
        self.assertIn("not a crate here", str(caught.exception))

    def test_does_not_apply_semver_to_sibling_requirements(self):
        # A patch of `borrow_lite_derive` is inside `borrow_lite`'s `^0.1.0`
        # requirement, so releasing only the sibling is legitimate. A hand-rolled
        # equality check here would wrongly refuse it; `cargo publish --dry-run`
        # applies the real rule.
        derive = self.manifests["borrow_lite_derive"]
        self.assertEqual(release.version_of(derive), "0.1.0")
        crate, version = release.check_tag(
            "borrow_lite_derive-v0.1.0", self.manifests, self.dirs
        )
        self.assertEqual(crate, "borrow_lite_derive")


class Pathspec(unittest.TestCase):
    """Notes are generated per crate, so the pathspec has to separate them."""

    def setUp(self):
        self.manifests, self.dirs = release.load_manifests()

    def test_the_root_crate_excludes_the_sibling_directory(self):
        pathspec = release.pathspec_for("borrow_lite", self.dirs)
        self.assertEqual(pathspec[0], ".")
        self.assertIn(":(exclude)derive", pathspec)

    def test_the_sibling_crate_is_its_own_directory(self):
        self.assertEqual(release.pathspec_for("borrow_lite_derive", self.dirs), ["derive"])


class PreviousTag(unittest.TestCase):
    """The baseline a release is compared against.

    `pick_tag` takes the tag list rather than shelling out to git, so the rule is
    tested without depending on which tags this checkout happens to have.
    """

    def test_picks_the_newest_tag_that_is_not_the_current_one(self):
        tags = ["borrow_lite-v0.3.0", "borrow_lite-v0.2.0", "borrow_lite-v0.1.0"]
        self.assertEqual(
            release.pick_tag(tags, "borrow_lite-v0.3.0"), "borrow_lite-v0.2.0"
        )

    def test_returns_none_when_only_the_excluded_tag_matches(self):
        self.assertIsNone(release.pick_tag(["borrow_lite-v0.1.0"], "borrow_lite-v0.1.0"))

    def test_returns_none_for_no_tags(self):
        self.assertIsNone(release.pick_tag([], "borrow_lite-v0.1.0"))


class TagDoesNotDrift(unittest.TestCase):
    """A guard on the repository itself, not on the parser."""

    def test_every_tag_shaped_ref_here_resolves(self):
        # Catches a tag created for a crate or version this repository does not
        # have, which is the mistake the workflow would refuse at release time.
        # Each tag is checked against the manifest at that tag's own commit: a
        # tag names a historical version, so the current manifest is the wrong
        # thing to compare it against once the version has moved on.
        tags = subprocess.run(
            ["git", "tag", "-l"],
            cwd=release.ROOT,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.split()
        for tag in tags:
            if tag.startswith("v"):
                continue
            manifests, dirs = release.load_manifests(ref=tag)
            release.check_tag(tag, manifests, dirs)

    def test_a_tag_is_read_from_its_own_commit(self):
        # The bug this pins: a tag checked against the *current* manifest looks
        # right until the version moves on, then fails - so every past release
        # breaks the guard the moment the next one is prepared. Reading the
        # manifest at the tag's own commit keeps it correct forever.
        tags = subprocess.run(
            ["git", "tag", "-l"],
            cwd=release.ROOT,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.split()
        crate_tags = [tag for tag in tags if not tag.startswith("v")]
        if not crate_tags:
            self.skipTest("no per-crate tags in this checkout")
        for tag in crate_tags:
            crate, tag_version = release.crates_from_tag(tag)
            manifests, _ = release.load_manifests(ref=tag)
            self.assertEqual(release.version_of(manifests[crate]), tag_version)


class Manifests(unittest.TestCase):
    """Reading the manifests, from the working tree or a historical ref."""

    def setUp(self):
        self.original_root = release.ROOT
        self.addCleanup(setattr, release, "ROOT", self.original_root)

    def _write(self, root: Path, relpath: str, text: str) -> None:
        path = root / relpath
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def test_a_missing_workspace_member_is_reported_not_raised_raw(self):
        # The original used `path.is_file()`; routing through `_manifest_text`
        # turned a missing member into a bare `FileNotFoundError` traceback on
        # the working-tree path. It has to stay a `ReleaseError` with the member
        # named, which is what the release command prints.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(
                root,
                "Cargo.toml",
                '[package]\nname = "borrow_lite"\nversion = "0.1.0"\n'
                '\n[workspace]\nmembers = ["derive"]\n',
            )
            release.ROOT = root
            with self.assertRaises(release.ReleaseError) as caught:
                release.load_manifests()
            self.assertIn("`derive` has no Cargo.toml", str(caught.exception))

    def test_an_inherited_version_resolves_against_the_root_it_was_read_with(self):
        # `version_of` used to re-read the *working tree* root, so a historical
        # tag with `version.workspace = true` would resolve against the current
        # version - the same drift the ref-aware read exists to prevent. Two
        # commits, a workspace bump between them, and the tag checked from the
        # older one.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._write(
                root,
                "Cargo.toml",
                '[package]\nname = "borrow_lite"\nversion = "0.1.0"\n'
                '\n[workspace]\nmembers = ["derive"]\n'
                '[workspace.package]\nversion = "0.1.0"\n',
            )
            self._write(
                root,
                "derive/Cargo.toml",
                '[package]\nname = "borrow_lite_derive"\nversion.workspace = true\n',
            )

            def git(*args):
                subprocess.run(["git", *args], cwd=root, check=True,
                               capture_output=True, text=True)

            git("init", "-q")
            git("config", "user.email", "t@example.com")
            git("config", "user.name", "t")
            git("add", "-A")
            git("commit", "-q", "-m", "0.1.0")
            git("tag", "borrow_lite-v0.1.0")

            # Bump the workspace version; the working tree now says 0.2.0.
            self._write(
                root,
                "Cargo.toml",
                '[package]\nname = "borrow_lite"\nversion = "0.2.0"\n'
                '\n[workspace]\nmembers = ["derive"]\n'
                '[workspace.package]\nversion = "0.2.0"\n',
            )
            git("add", "-A")
            git("commit", "-q", "-m", "0.2.0")

            release.ROOT = root
            historical, _ = release.load_manifests(ref="borrow_lite-v0.1.0")
            current, _ = release.load_manifests()
            self.assertEqual(release.version_of(historical["borrow_lite"]), "0.1.0")
            self.assertEqual(release.version_of(current["borrow_lite"]), "0.2.0")
            # The inherited one is where the drift showed: re-reading the working
            # tree root returned the current 0.2.0 for a 0.1.0 tag.
            self.assertEqual(
                release.version_of(historical["borrow_lite_derive"]), "0.1.0"
            )
            self.assertEqual(
                release.version_of(current["borrow_lite_derive"]), "0.2.0"
            )
            # The historical tag still validates against its own root.
            release.check_tag("borrow_lite_derive-v0.1.0", historical, {})


if __name__ == "__main__":
    unittest.main()
