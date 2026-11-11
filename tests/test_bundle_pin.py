"""Exercise manifest-only updates against local Git remotes, without publishing."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class BundlePinTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="kureha bundle git ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.remote = self.root / "remote.git"
        self.author = self.root / "author"
        self.builder = self.root / "builder"
        self.git(self.root, "init", "--bare", "--initial-branch=main", str(self.remote))
        self.git(self.root, "clone", str(self.remote), str(self.author))
        self.configure(self.author)
        (self.author / "source.txt").write_text("source for release\n")
        self.git(self.author, "add", ".")
        self.git(self.author, "commit", "-m", "Initial source")
        self.git(self.author, "push", "origin", "main")
        self.revision = self.git(self.author, "rev-parse", "HEAD").stdout.strip()
        self.git(self.root, "clone", str(self.remote), str(self.builder))
        self.configure(self.builder)
        self.git(self.builder, "checkout", "--detach", self.revision)

    def git(self, directory, *arguments, check=True):
        return subprocess.run(["git", "-c", "commit.gpgsign=false", *arguments],
                              cwd=directory, capture_output=True, text=True, check=check)

    def configure(self, directory):
        self.git(directory, "config", "user.name", "Release test")
        self.git(directory, "config", "user.email", "release@example.test")

    def write_bundle(self, checkout, contents):
        path = checkout / "platforms/scarlet/bundle.toml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)

    def pin(self):
        return subprocess.run(["bash", str(ROOT / "scripts/pin-scarlet-bundle.sh"),
                               self.revision, "scarlet-test"], cwd=self.builder,
                              capture_output=True, text=True,
                              env={**os.environ, "GIT_CONFIG_COUNT": "1",
                                   "GIT_CONFIG_KEY_0": "commit.gpgsign",
                                   "GIT_CONFIG_VALUE_0": "false"})

    def test_first_bundle_is_committed_without_changing_source(self):
        self.write_bundle(self.builder, "verified archive manifest\n")
        result = self.pin()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git(self.remote, "show", "main:platforms/scarlet/bundle.toml").stdout,
                         "verified archive manifest\n")
        self.assertEqual(self.git(self.remote, "show", "main:source.txt").stdout, "source for release\n")

    def test_new_main_commits_and_previous_release_are_preserved(self):
        (self.author / "source.txt").write_text("newer source stays intact\n")
        self.write_bundle(self.author, "previous release\n")
        self.git(self.author, "add", ".")
        self.git(self.author, "commit", "-m", "Changes while release builds")
        main = self.git(self.author, "rev-parse", "HEAD").stdout.strip()
        self.git(self.author, "push", "origin", "main")
        self.write_bundle(self.builder, "verified archive manifest\n")
        result = self.pin()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git(self.remote, "show", "main:source.txt").stdout, "newer source stays intact\n")
        self.assertEqual(self.git(self.remote, "rev-parse", "main^").stdout.strip(), main)
        self.assertEqual(self.git(self.remote, "diff-tree", "--no-commit-id", "--name-only", "-r", "main").stdout,
                         "platforms/scarlet/bundle.toml\n")
        self.assertEqual(self.git(self.remote, "show", "main:platforms/scarlet/bundle.toml").stdout,
                         "verified archive manifest\n")

    def test_rewritten_main_is_not_overwritten(self):
        self.git(self.author, "checkout", "--orphan", "replacement")
        self.git(self.author, "commit", "-m", "Unrelated replacement history")
        self.git(self.author, "push", "--force", "origin", "HEAD:main")
        main = self.git(self.remote, "rev-parse", "main").stdout.strip()
        self.write_bundle(self.builder, "verified archive manifest\n")
        result = self.pin()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no longer an ancestor", result.stderr)
        self.assertEqual(self.git(self.remote, "rev-parse", "main").stdout.strip(), main)


if __name__ == "__main__":
    unittest.main()
