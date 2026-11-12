"""Test archive composition and release integrity without cross-compiling the editor."""
import importlib.util
import io
import json
from pathlib import Path
import shutil
import struct
import tarfile
import tempfile
import tomllib
import unittest
import zipfile
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

    def test_first_publish_creates_bundle_directory_after_asset_verification(self):
        output = self.assemble()
        checkout = self.root / "fresh checkout"
        checkout.mkdir()

        def run(command, **kwargs):
            if command[:3] == ["gh", "release", "download"]:
                downloaded = Path(command[command.index("--dir") + 1])
                for source in output.iterdir():
                    shutil.copyfile(source, downloaded / source.name)

        with patch.object(release, "ROOT", checkout), \
                patch.object(release.subprocess, "check_output", return_value="a" * 40), \
                patch.object(release.subprocess, "run", side_effect=run) as commands:
            release.publish(output, prerelease=True)
        self.assertEqual(commands.call_count, 2)
        self.assertEqual((checkout / "platforms/scarlet/bundle.toml").read_bytes(),
                         (output / "bundle.toml").read_bytes())


def linux_elf(machine, loader, extra=b"", needed=("libfontconfig.so.1", "libc.so.6"), runpath=None):
    """A minimal ELF64 with PT_INTERP, one PT_LOAD and a dynamic section."""
    interp = loader.encode() + b"\0"
    strings = b"\0"
    offsets = {}
    for name in [*needed, *([runpath] if runpath else [])]:
        offsets[name] = len(strings)
        strings += name.encode() + b"\0"
    phoff, phnum = 64, 3
    interp_at = phoff + 56 * phnum
    strtab_at = interp_at + len(interp)
    dynamic_at = (strtab_at + len(strings) + 7) // 8 * 8
    entries = [(1, offsets[name]) for name in needed]
    if runpath:
        entries.append((29, offsets[runpath]))
    entries += [(5, strtab_at), (0, 0)]
    dynamic = b"".join(struct.pack("<qQ", tag, value) for tag, value in entries)
    tail = b"GLIBC_2.17\0GLIBC_2.39\0GLIBC_2.4\0" + extra
    size = dynamic_at + len(dynamic) + len(tail)
    data = bytearray(dynamic_at)
    data[:8] = b"\x7fELF\x02\x01\x01\x00"
    struct.pack_into("<HH", data, 16, 3, machine)
    struct.pack_into("<Q", data, 32, phoff)
    struct.pack_into("<HH", data, 54, 56, phnum)
    for index, (kind, offset, length) in enumerate([
            (3, interp_at, len(interp)), (1, 0, size), (2, dynamic_at, len(dynamic))]):
        at = phoff + 56 * index
        struct.pack_into("<I", data, at, kind)
        struct.pack_into("<QQ", data, at + 8, offset, offset)  # p_offset, p_vaddr
        struct.pack_into("<Q", data, at + 32, length)  # p_filesz
    data[interp_at:interp_at + len(interp)] = interp
    data[strtab_at:strtab_at + len(strings)] = strings
    return bytes(data) + dynamic + tail


class DesktopReleaseTests(ScarletReleaseTests):
    """Desktop packages share the release and checksums but never bundle.toml."""

    def setUp(self):
        super().setUp()
        for name in release.DESKTOP:
            self.make_desktop(name)

    def make_desktop(self, name, executable=None, mode=0o755, revision="a" * 40, extra=None):
        platform, arch = release.DESKTOP[name]
        archive = self.inputs / release.desktop_archive_name(name, self.version)
        if platform == "macos":
            data = executable or b"\xcf\xfa\xed\xfe" + struct.pack("<I", release.MACHO_ARM64) + bytes(24)
            with zipfile.ZipFile(archive, "w") as stream:
                for entry, payload, entry_mode in [
                        (release.MACOS_EXECUTABLE, data, mode),
                        (release.MACOS_APP + "/Contents/Info.plist", b"<plist/>", 0o644),
                        *(extra or [])]:
                    info = zipfile.ZipInfo(entry)
                    info.external_attr = (0o100000 | entry_mode) << 16
                    stream.writestr(info, payload)
        else:
            machine, loader = release.LINUX_ELF[arch]
            data = executable or linux_elf(machine, loader)
            with tarfile.open(archive, "w:gz") as stream:
                for entry, payload, entry_mode in [
                        (release.LINUX_EXECUTABLE, data, mode),
                        (release.LINUX_DIR + "/share/applications/kureha-code.desktop", b"[Desktop Entry]\n", 0o644),
                        *(extra or [])]:
                    info = tarfile.TarInfo(entry)
                    info.size, info.mode = len(payload), entry_mode
                    stream.addfile(info, io.BytesIO(payload))
        record = {"version": self.version, "name": name, "platform": platform, "arch": arch,
                  "revision": revision, "dirty": False, "external_artifact": False,
                  "archive": archive.name, "sha256": release.sha(archive),
                  "cargo_lock_sha256": "b" * 64, "flake_lock_sha256": "c" * 64}
        glibc = release.verify_desktop_archive(archive, name)
        if platform == "linux":
            record["glibc"] = glibc
        release.write_json(self.inputs / f"desktop-{name}.json", record)

    def assemble(self, desktop=True):
        output = self.root / ("release" if desktop else "scarlet-only")
        release.assemble(self.inputs, self.version, output, desktop)
        return output

    def test_desktop_packages_join_the_release_but_not_the_bundle(self):
        output = self.assemble()
        release.verify_release(output)
        scarlet_only = self.assemble(desktop=False)
        self.assertEqual((output / "bundle.toml").read_bytes(),
                         (scarlet_only / "bundle.toml").read_bytes())
        sums = (output / "SHA256SUMS").read_text()
        manifest = json.loads((output / "release.json").read_text())
        self.assertEqual([r["name"] for r in manifest["desktop"]], list(release.DESKTOP))
        for name in release.DESKTOP:
            archive = release.desktop_archive_name(name, self.version)
            self.assertIn(f"{release.sha(output / archive)}  {archive}", sums)
            self.assertNotIn(archive, (output / "bundle.toml").read_text())
        self.assertEqual(manifest["desktop"][1]["glibc"], "2.39")
        notes = release.release_notes(manifest)
        self.assertIn("glibc 2.39 or newer", notes)
        self.assertIn("xattr -dr com.apple.quarantine", notes)

    def test_linux_binaries_must_run_on_the_system_loader_and_libraries(self):
        machine, loader = release.LINUX_ELF["aarch64"]
        for executable, message in [
                (linux_elf(machine, "/nix/store/abc-glibc-2.42/lib/ld-linux-aarch64.so.1"), "loader"),
                (linux_elf(machine, "/lib/ld-musl-aarch64.so.1"), "loader"),
                (linux_elf(62, "/lib64/ld-linux-x86-64.so.2"), "aarch64 Linux ELF"),
                (linux_elf(machine, loader, runpath="/nix/store/abc-glibc-2.42/lib"), "search path"),
                (linux_elf(machine, loader, needed=("/nix/store/abc-fontconfig/lib/libfontconfig.so.1",)),
                 "by path")]:
            with self.subTest(message=message):
                with self.assertRaisesRegex(ValueError, message):
                    self.make_desktop("linux-aarch64", executable=executable)
        # Unused bytes, such as a removed RUNPATH or a library's last-resort
        # search directory, may still mention the Nix store.
        self.make_desktop("linux-aarch64", executable=linux_elf(
            machine, loader, extra=b"/nix/store/abc-libx11-1.8/lib\0"))
        self.assemble()
        with self.assertRaisesRegex(ValueError, "Nix store"):
            self.make_desktop("linux-aarch64", extra=[
                (release.LINUX_DIR + "/share/kureha.desktop", b"Exec=/nix/store/x/bin/kureha-code", 0o644)])

    def test_macos_packages_must_not_mention_the_nix_store(self):
        with self.assertRaisesRegex(ValueError, "Nix store"):
            self.make_desktop("macos-aarch64", extra=[
                (release.MACOS_APP + "/Contents/Frameworks/libfoo.dylib", b"/nix/store/q-foo", 0o644)])
        with self.assertRaisesRegex(ValueError, "arm64 Mach-O"):
            self.make_desktop("macos-aarch64", executable=b"\xcf\xfa\xed\xfe" + bytes(28))

    def test_desktop_layout_and_permissions_are_checked(self):
        with self.assertRaisesRegex(ValueError, "executable permissions"):
            self.make_desktop("linux-x86_64", mode=0o644)
        with self.assertRaisesRegex(ValueError, "invalid desktop archive member"):
            self.make_desktop("macos-aarch64", extra=[("../escape", b"x", 0o644)])

    def test_desktop_builds_must_match_the_scarlet_source(self):
        self.make_desktop("linux-x86_64", revision="d" * 40)
        with self.assertRaisesRegex(ValueError, "disagree on revision"):
            self.assemble()

    def test_missing_desktop_build_is_rejected(self):
        (self.inputs / "desktop-macos-aarch64.json").unlink()
        with self.assertRaises(OSError):
            self.assemble()

    def test_dirty_desktop_build_cannot_be_published(self):
        path = self.inputs / "desktop-linux-aarch64.json"
        record = json.loads(path.read_text())
        record["dirty"] = True
        release.write_json(path, record)
        output = self.assemble()
        with patch.object(release.subprocess, "check_output", return_value="a" * 40), \
                patch.object(release.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "clean source builds"):
                release.publish(output, prerelease=True)
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
