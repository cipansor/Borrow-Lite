"""Tests for the release tag resolver.

The rules covered here are the ones that fail quietly: a tag that names the wrong
crate, or a tag whose version disagrees with the manifest. Neither breaks a
build, so nothing else in CI would catch them - the mistake surfaces at the
upload, which cannot be undone.
"""

import importlib.util
import subprocess
import sys
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
        manifests, dirs = release.load_manifests()
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
            release.check_tag(tag, manifests, dirs)


if __name__ == "__main__":
    unittest.main()
