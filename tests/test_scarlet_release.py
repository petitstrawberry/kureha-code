"""Test archive composition and release integrity without cross-compiling the editor."""
import importlib.util
import io
import json
from pathlib import Path
import struct
import tarfile
import tempfile
import tomllib
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("scarlet_release", ROOT / "scripts/release-scarlet.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ScarletReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="kureha release ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.inputs = self.root / "inputs"
        self.inputs.mkdir()
        self.version = "scarlet-v0.1.0-42.1"
        for target in release.TARGETS:
            self.make_input(target)

    def make_input(self, target, machine=None, executable_mode=0o755, extra=None):
        arch, expected_machine = release.TARGETS[target]
        archive = self.inputs / f"kureha-code-{arch}-{self.version}.tar.gz"
        header = bytearray(64)
        header[:8] = b"\x7fELF\x02\x01\x01\x53"
        struct.pack_into("<HH", header, 16, 3, expected_machine if machine is None else machine)
        descriptor = ("[Desktop Entry]\nType=Application\nName=Kureha Code\n"
                      "Exec=bin/kureha-code %f\nX-Scarlet-AppFormat=1\n"
                      f"X-Scarlet-Target={target}\n").encode()
        files = [(release.EXECUTABLE, bytes(header), executable_mode),
                 (release.APP_PATH + "/org.scarlet-os.kureha-code.desktop", descriptor, 0o644),
                 (release.APP_PATH + "/resources/licenses/Kureha-Code.txt", b"license\n", 0o644)]
        with tarfile.open(archive, "w:gz") as stream:
            for name, data, mode in files:
                info = tarfile.TarInfo(name)
                info.size, info.mode = len(data), mode
                stream.addfile(info, io.BytesIO(data))
            if extra:
                stream.addfile(extra)
        record = {"version": self.version, "target": target, "arch": arch,
                  "revision": "a" * 40, "dirty": False, "external_artifact": False,
                  "archive": archive.name, "sha256": release.sha(archive),
                  "cargo_lock_sha256": "b" * 64, "flake_lock_sha256": "c" * 64}
        release.write_json(self.inputs / f"{arch}.json", record)
        return archive

    def assemble(self):
        output = self.root / "release"
        release.assemble(self.inputs, self.version, output)
        return output

    def test_complete_release_uses_archives_with_verified_hashes_and_runtime_layout(self):
        output = self.assemble()
        release.verify_release(output)
        bundle = tomllib.loads((output / "bundle.toml").read_text())
        self.assertEqual(len(bundle["layers"]), 1)
        layer = bundle["layers"][0]
        self.assertEqual((layer["kind"], layer["format"], layer["to"]), ("archive", "tar-gz", "/"))
        for arch in ("aarch64", "riscv64"):
            name = f"kureha-code-{arch}-{self.version}.tar.gz"
            self.assertTrue(layer["url"].replace("{arch}", arch).endswith("/" + name))
            self.assertEqual(layer["sha256"][arch], "sha256:" + release.sha(output / name))
            stage = self.root / arch
            with tarfile.open(output / name) as stream:
                stream.extractall(stage, filter="data")
            self.assertTrue((stage / release.EXECUTABLE).is_file())
            self.assertEqual((stage / release.APP_PATH / "resources/licenses/Kureha-Code.txt").read_text(), "license\n")

    def test_wrong_architecture_and_missing_executable_permission_are_rejected(self):
        for options in ({"machine": 62}, {"executable_mode": 0o644}):
            with self.subTest(options=options):
                self.make_input("aarch64-unknown-scarlet", **options)
                with self.assertRaises(ValueError):
                    self.assemble()

    def test_archive_cannot_write_outside_app_or_contain_links(self):
        for name, kind in [("../escape", tarfile.DIRTYPE),
                           ("applications/other.app", tarfile.DIRTYPE),
                           (release.APP_PATH + "/link", tarfile.SYMTYPE)]:
            with self.subTest(name=name):
                extra = tarfile.TarInfo(name)
                extra.type, extra.linkname = kind, "/tmp/outside"
                self.make_input("aarch64-unknown-scarlet", extra=extra)
                with self.assertRaisesRegex(ValueError, "invalid app archive member"):
                    self.assemble()

    def test_corrupt_build_download_is_rejected(self):
        archive = next(self.inputs.glob("*.tar.gz"))
        with archive.open("ab") as stream:
            stream.write(b"changed in transfer")
        with self.assertRaisesRegex(ValueError, "input mismatch"):
            self.assemble()

    def test_mixed_source_revisions_are_rejected(self):
        path = self.inputs / "riscv64.json"
        record = json.loads(path.read_text())
        record["revision"] = "d" * 40
        release.write_json(path, record)
        with self.assertRaisesRegex(ValueError, "disagree on revision"):
            self.assemble()

    def test_modified_manifest_is_rejected_even_with_updated_checksum_file(self):
        output = self.assemble()
        manifest = output / "bundle.toml"
        manifest.write_text(manifest.read_text().replace('to = "/"', 'to = "/wrong"'))
        (output / "SHA256SUMS").write_text("".join(
            f"{release.sha(path)}  {path.name}\n" for path in sorted(output.iterdir())
            if path.name != "SHA256SUMS"))
        with self.assertRaisesRegex(ValueError, "verification failed"):
            release.verify_release(output)

    def test_unexpected_release_files_are_rejected(self):
        output = self.assemble()
        (output / "unexpected").write_text("extra")
        with self.assertRaisesRegex(ValueError, "unexpected release files"):
            release.verify_release(output)

    def test_dirty_or_external_builds_cannot_be_published(self):
        for flag in ("dirty", "external_artifact"):
            with self.subTest(flag=flag):
                path = self.inputs / "aarch64.json"
                record = json.loads(path.read_text())
                record[flag] = True
                release.write_json(path, record)
                output = self.root / flag
                release.assemble(self.inputs, self.version, output)
                with patch.object(release.subprocess, "check_output", return_value="a" * 40), \
                        patch.object(release.subprocess, "run") as run:
                    with self.assertRaisesRegex(ValueError, "clean source builds"):
                        release.publish(output, prerelease=True)
                    run.assert_not_called()
                record[flag] = False
                release.write_json(path, record)


if __name__ == "__main__":
    unittest.main()
