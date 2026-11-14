"""Exercise the extension build script with a stand-in for Cargo."""
import importlib.util
import io
import json
import os
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("build_extension", ROOT / "scripts/build-extension.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)

MODULE = b"\0asm\1\0\0\0"


class BuildExtensionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="kureha extension build ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source dir" / "tool"
        (self.source / "src").mkdir(parents=True)
        (self.source / "themes").mkdir()
        (self.source / "src/lib.rs").write_text("// source\n")
        (self.source / "Cargo.toml").write_text("[package]\nname = 'tool'\n")
        (self.source / "Cargo.lock").write_text("")
        (self.source / "README.md").write_text("# Tool\n")
        (self.source / "LICENSE-MIT").write_text("MIT\n")
        (self.source / "notes.txt").write_text("not shipped\n")
        (self.source / "themes/dark.toml").write_text("name = 'Dark'\n")
        self.write_manifest()
        self.out = self.root / "out dir"

    def write_manifest(self, id="test.tool", wasm="extension.wasm", theme="themes/dark.toml"):
        lines = ["[extension]", f"id = '{id}'", "name = 'Tool'", "version = '1'", "api = 1"]
        if wasm is not None:
            lines.append(f"wasm = '{wasm}'")
        if theme is not None:
            lines += ["[[contributes.themes]]", f"path = '{theme}'"]
        (self.source / "extension.toml").write_text("\n".join(lines) + "\n")

    def fake_cargo(self, status=0, module=MODULE):
        """A `cargo` that reports the SDK, then the crate's cdylib in a target
        folder with a space, as Cargo's JSON messages do."""
        target = self.root / "target dir"
        target.mkdir(exist_ok=True)
        cargo = self.root / "bin" / "cargo"
        cargo.parent.mkdir(exist_ok=True)
        sdk = self.root / "sdk/Cargo.toml"
        script = f"""#!{sys.executable}
import json, pathlib, sys
manifest = sys.argv[sys.argv.index("--manifest-path") + 1]
target = pathlib.Path({str(target)!r})
module = target / "tool.wasm"
module.write_bytes({module!r})
print(json.dumps({{"reason": "compiler-artifact", "manifest_path": {str(sdk)!r},
                   "target": {{"kind": ["lib"]}}, "filenames": [str(target / "libsdk.rlib")]}}))
print(json.dumps({{"reason": "compiler-artifact", "manifest_path": manifest,
                   "target": {{"kind": ["cdylib"]}}, "filenames": [str(module)]}}))
sys.exit({status})
"""
        cargo.write_text(script)
        cargo.chmod(0o755)
        return patch.dict(os.environ, {"PATH": str(cargo.parent) + os.pathsep + os.environ["PATH"]})

    def run_main(self, *args):
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            status = build.main([*map(str, args)])
        return status, stdout.getvalue(), stderr.getvalue()

    def installed(self, folder):
        return sorted(str(path.relative_to(folder)) for path in folder.rglob("*") if path.is_file())

    def test_the_folder_holds_only_what_kureha_loads(self):
        with self.fake_cargo():
            status, stdout, stderr = self.run_main("--out", self.out, self.source)
        self.assertEqual(status, 0, stderr)
        folder = self.out / "tool.kx"
        self.assertIn(str(folder), stdout)
        self.assertEqual(self.installed(folder), [
            "LICENSE-MIT", "README.md", "extension.toml", "extension.wasm", "themes/dark.toml"])
        self.assertEqual((folder / "extension.wasm").read_bytes(), MODULE)
        self.assertEqual((folder / "extension.toml").read_text(),
                         (self.source / "extension.toml").read_text())
        self.assertEqual([p.name for p in self.out.iterdir()], ["tool.kx"], "no staging left")

    def test_a_failed_build_keeps_the_installed_extension(self):
        with self.fake_cargo():
            self.assertEqual(self.run_main("--out", self.out, self.source)[0], 0)
        with self.fake_cargo(status=1, module=b"broken"):
            status, _, stderr = self.run_main("--out", self.out, self.source)
        self.assertEqual(status, 1)
        self.assertIn("Cargo build failed", stderr)
        self.assertEqual((self.out / "tool.kx/extension.wasm").read_bytes(), MODULE)
        self.assertEqual([p.name for p in self.out.iterdir()], ["tool.kx"])

    def test_a_rebuild_replaces_the_folder(self):
        with self.fake_cargo():
            self.assertEqual(self.run_main("--out", self.out, self.source)[0], 0)
        (self.out / "tool.kx/stale.txt").write_text("from an older version\n")
        with self.fake_cargo(module=MODULE + b"v2"):
            self.assertEqual(self.run_main("--out", self.out, self.source)[0], 0)
        self.assertFalse((self.out / "tool.kx/stale.txt").exists())
        self.assertEqual((self.out / "tool.kx/extension.wasm").read_bytes(), MODULE + b"v2")
        self.assertEqual([p.name for p in self.out.iterdir()], ["tool.kx"])

    def test_a_linked_source_folder_is_replaced_without_touching_the_source(self):
        self.out.mkdir()
        (self.out / "tool").symlink_to(self.source)  # the old instructions
        (self.out / "tool.kx").symlink_to(self.source)
        with self.fake_cargo():
            status, stdout, stderr = self.run_main("--out", self.out, self.source)
        self.assertEqual(status, 0, stderr)
        self.assertIn("Removed", stdout)
        self.assertEqual([p.name for p in self.out.iterdir()], ["tool.kx"])
        self.assertFalse((self.out / "tool.kx").is_symlink())
        self.assertTrue((self.source / "src/lib.rs").is_file())
        self.assertFalse((self.source / "extension.wasm").exists())

    def test_another_folder_with_the_same_id_is_left_alone(self):
        other = self.out / "copy"
        other.mkdir(parents=True)
        (other / "extension.toml").write_text((self.source / "extension.toml").read_text())
        with self.fake_cargo():
            status, _, stderr = self.run_main("--out", self.out, self.source)
        self.assertEqual(status, 1)
        self.assertIn("also holds test.tool", stderr)
        self.assertEqual([p.name for p in self.out.iterdir()], ["copy"])

    def test_a_folder_of_another_extension_is_not_replaced(self):
        taken = self.out / "tool.kx"
        taken.mkdir(parents=True)
        (taken / "extension.toml").write_text("[extension]\nid = 'someone.else'\n")
        with self.fake_cargo():
            status, _, stderr = self.run_main("--out", self.out, self.source)
        self.assertEqual(status, 1)
        self.assertIn("holds something else", stderr)
        self.assertEqual((taken / "extension.toml").read_text(), "[extension]\nid = 'someone.else'\n")

    def test_manifest_paths_must_stay_inside_and_exist(self):
        for wasm, theme, expected in [
            ("../escape.wasm", None, "inside the extension folder"),
            ("/abs/extension.wasm", None, "inside the extension folder"),
            ("extension.wasm", "../dark.toml", "inside the extension folder"),
            ("extension.wasm", "themes/missing.toml", "missing"),
        ]:
            self.write_manifest(wasm=wasm, theme=theme)
            with self.fake_cargo():
                status, _, stderr = self.run_main("--out", self.out, self.source)
            self.assertEqual(status, 1, (wasm, theme))
            self.assertIn(expected, stderr, (wasm, theme))
            self.assertFalse(self.out.exists() and any(self.out.iterdir()), (wasm, theme))

    def test_a_declaration_only_extension_needs_no_cargo(self):
        self.write_manifest(wasm=None)
        with patch.dict(os.environ, {"PATH": str(self.root / "nowhere")}):
            status, _, stderr = self.run_main("--out", self.out, self.source)
        self.assertEqual(status, 0, stderr)
        self.assertNotIn("extension.wasm", self.installed(self.out / "tool.kx"))

    def test_install_uses_kureha_extensions_folder(self):
        with self.fake_cargo(), patch.dict(os.environ, {"KUREHA_EXTENSIONS_DIR": str(self.out)}):
            status, stdout, stderr = self.run_main("--install", self.source)
        self.assertEqual(status, 0, stderr)
        self.assertTrue((self.out / "tool.kx/extension.wasm").is_file())
        self.assertIn("Restart Kureha Code", stdout)
        with patch.dict(os.environ, {"KUREHA_EXTENSIONS_DIR": "", "XDG_CONFIG_HOME": "/cfg"}):
            self.assertEqual(build.kureha_extensions_dir(), Path("/cfg/kureha-code/extensions"))

    def test_every_extension_has_a_manifest_naming_the_installed_module(self):
        manifests = [m for root in build.SOURCES for m in sorted(root.glob("*/extension.toml"))]
        self.assertIn(ROOT / "extensions/direnv/extension.toml", manifests)
        for manifest in manifests:
            self.assertEqual(build.read_manifest(manifest.parent)["extension"].get("wasm"),
                             "extension.wasm", manifest)


if __name__ == "__main__":
    unittest.main()
