"""Exercise the Cargo adapter and the real Appcoat CLI without rebuilding the editor."""
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("package", ROOT / "scripts/package.py")
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="kureha packaging ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.artifact = self.root / "custom target" / "kureha-code"
        self.artifact.parent.mkdir()
        self.artifact.write_text('#!/bin/sh\nprintf "arg=%s\\n" "$@"\n')
        self.artifact.chmod(0o755)
        if sys.platform == "darwin":
            source = self.root / "main.c"
            source.write_text('#include <stdio.h>\nint main(int argc, char **argv) { for (int i=1;i<argc;i++) printf("arg=%s\\n",argv[i]); }\n')
            subprocess.run(["/usr/bin/cc", str(source), "-o", str(self.artifact), "-Wl,-headerpad_max_install_names"], check=True, capture_output=True)

    def fake_cargo(self, status):
        cargo = self.root / "cargo"
        messages = [
            {"reason": "compiler-artifact", "target": {"name": "kureha_code", "kind": ["lib"]}},
            {"reason": "compiler-artifact", "target": {"name": "kureha-code", "kind": ["bin"]},
             "executable": str(self.artifact)},
        ]
        cargo.write_text(f"#!{sys.executable}\nimport sys\nprint({repr(chr(10).join(map(json.dumps, messages)))})\nsys.exit({status})\n")
        cargo.chmod(0o755)
        return patch.dict(os.environ, {"PATH": str(self.root) + os.pathsep + os.environ["PATH"]})

    def test_cargo_report_handles_external_target_directory_with_spaces(self):
        with self.fake_cargo(0):
            self.assertEqual(package.build_artifact(self.root), self.artifact.resolve())

    def test_failed_cargo_does_not_reuse_reported_artifact(self):
        with self.fake_cargo(1):
            with self.assertRaisesRegex(RuntimeError, "Cargo build failed"):
                package.build_artifact(self.root)

    def test_package_from_other_directory_preserves_existing_destination(self):
        platform = "macos" if sys.platform == "darwin" else "linux"
        output = self.root / "Kureha Code.app"
        command = [sys.executable, str(ROOT / "scripts/package.py"), "--platform", platform,
                   "--artifact", str(self.artifact.relative_to(self.root)), "--output", output.name]
        result = subprocess.run(command, cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        if platform == "macos":
            with (output / "Contents/Info.plist").open("rb") as stream:
                plist = plistlib.load(stream)
            self.assertEqual(plist["CFBundleIdentifier"], "org.scarlet-os.kureha-code")
            self.assertEqual(plist["CFBundleName"], "Kureha Code")
            self.assertTrue((output / "Contents/Resources" / plist["CFBundleIconFile"]).is_file())
        else:
            self.assertTrue((output / "share/pixmaps/org.scarlet-os.kureha-code.png").is_file())
        license_paths = list(output.rglob("gruvbox.LICENSE"))
        self.assertEqual(len(license_paths), 1)
        self.assertEqual(license_paths[0].read_bytes(), (ROOT / "assets/themes/gruvbox.LICENSE").read_bytes())
        catalogues = list(output.rglob("acknowledgements.json"))
        self.assertEqual(len(catalogues), 1)
        catalogue = json.loads(catalogues[0].read_text())
        gruvbox = next(entry for entry in catalogue["entries"] if entry["name"] == "Gruvbox palette")
        self.assertIn(license_paths[0].read_text().strip(),
                      {catalogue["texts"][index].strip() for index in gruvbox["texts"]})
        launched = subprocess.run(["appcoat", "run", "--package", str(output), "--", "日本語 file.md"],
                                 capture_output=True, text=True)
        self.assertEqual(launched.returncode, 0, launched.stderr)
        self.assertEqual(launched.stdout, "arg=日本語 file.md\n")
        sentinel = output / "keep"
        sentinel.write_text("keep")
        # No artifact/build work may run when the output already exists.
        self.artifact.unlink()
        again = subprocess.run(command, cwd=self.root, capture_output=True, text=True)
        self.assertNotEqual(again.returncode, 0)
        self.assertIn("destination already exists", again.stderr)
        self.assertEqual(sentinel.read_text(), "keep")

    def test_scarlet_target_is_required_and_host_target_is_rejected(self):
        for args in (["--platform", "scarlet"],
                     ["--platform", "macos", "--target", "aarch64-unknown-scarlet"]):
            with self.subTest(args=args):
                result = subprocess.run([sys.executable, str(ROOT / "scripts/package.py"), *args],
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("--target is required only", result.stderr)


if __name__ == "__main__":
    unittest.main()
