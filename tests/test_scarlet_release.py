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
import unittest.mock
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
        self.version = "v0.1.0-42.1"
        for target in release.TARGETS:
            self.make_input(target)

    def make_input(self, target, machine=None, executable_mode=0o755, extra=None):
        arch, expected_machine = release.TARGETS[target]
        archive = self.inputs / release.scarlet_archive_name(arch, self.version)
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
            name = release.scarlet_archive_name(arch, self.version)
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

    def make_desktop(self, name, executable=None, mode=0o755, revision="a" * 40, extra=None,
                     signature="ad-hoc", notarized=False, image=None):
        platform, arch = release.DESKTOP[name]
        archive = self.inputs / release.desktop_archive_name(name, self.version)
        if platform == "macos":
            # Contents are checked on macOS at build time (see macos_app below);
            # elsewhere only the UDIF trailer can be read.
            archive.write_bytes(udif_image() if image is None else image)
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
        else:
            record["signature"], record["notarized"] = signature, notarized
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
        self.assertTrue(manifest["desktop"][0]["archive"].endswith(".dmg"))
        # Every package is kureha-code-<platform>-<arch>-<tag>.
        packages = sorted(path.name for path in output.iterdir() if path.name.startswith("kureha-code-"))
        self.assertEqual(packages, [
            f"kureha-code-{platform}-{arch}-{self.version}.{extension}" for platform, arch, extension in [
                ("linux", "aarch64", "tar.gz"), ("linux", "x86_64", "tar.gz"), ("macos", "aarch64", "dmg"),
                ("scarlet", "aarch64", "tar.gz"), ("scarlet", "riscv64", "tar.gz")]])
        notes = release.release_notes(manifest)
        self.assertIn("glibc 2.39 or newer", notes)
        self.assertIn("kureha-code-macos-aarch64-*.dmg", notes)
        self.assertIn("xattr -dr com.apple.quarantine", notes)

    def test_notarized_builds_drop_the_quarantine_workaround(self):
        self.make_desktop("macos-aarch64", signature="developer-id", notarized=True)
        notes = release.release_notes(json.loads((self.assemble() / "release.json").read_text()))
        self.assertIn("notarized by Apple", notes)
        self.assertNotIn("xattr", notes)

    def test_macos_signature_record_must_be_consistent(self):
        for signature, notarized in (("developer-id", False), ("ad-hoc", True), (None, None)):
            with self.subTest(signature=signature):
                self.make_desktop("macos-aarch64", signature=signature, notarized=notarized)
                with self.assertRaisesRegex(ValueError, "input mismatch: macos"):
                    self.assemble()

    def test_macos_asset_must_be_a_disk_image(self):
        for image in (b"PK\x03\x04" + bytes(600), b"koly", udif_image(version=3)):
            with self.subTest(image=image[:8]):
                with self.assertRaisesRegex(ValueError, "UDIF disk image"):
                    self.make_desktop("macos-aarch64", image=image)

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

    def macos_app(self, executable=None, mode=0o755, extra=()):
        """An app tree as mounted from the disk image, checked like an archive."""
        volume = self.root / "volume"
        shutil.rmtree(volume, ignore_errors=True)
        data = executable or b"\xcf\xfa\xed\xfe" + struct.pack("<I", release.MACHO_ARM64) + bytes(24)
        for entry, payload, entry_mode in [(release.MACOS_EXECUTABLE, data, mode),
                                           (release.MACOS_APP + "/Contents/Info.plist", b"<plist/>", 0o644),
                                           *extra]:
            path = volume / entry
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(payload)
            path.chmod(entry_mode)
        return release.check_desktop_files(release.tree_files(volume, release.MACOS_APP), "macos-aarch64")

    def test_macos_apps_must_not_mention_the_nix_store(self):
        self.assertIsNone(self.macos_app())
        with self.assertRaisesRegex(ValueError, "Nix store"):
            self.macos_app(extra=[
                (release.MACOS_APP + "/Contents/Frameworks/libfoo.dylib", b"/nix/store/q-foo", 0o644)])
        with self.assertRaisesRegex(ValueError, "arm64 Mach-O"):
            self.macos_app(executable=b"\xcf\xfa\xed\xfe" + bytes(28))
        with self.assertRaisesRegex(ValueError, "executable permissions"):
            self.macos_app(mode=0o644)

    def test_macos_apps_cannot_contain_links(self):
        self.macos_app()
        (self.root / "volume" / release.MACOS_APP / "Contents/Resources").mkdir()
        (self.root / "volume" / release.MACOS_APP / "Contents/Resources/link").symlink_to("/etc/passwd")
        with self.assertRaisesRegex(ValueError, "invalid desktop package entry"):
            release.tree_files(self.root / "volume", release.MACOS_APP)

    def test_desktop_layout_and_permissions_are_checked(self):
        with self.assertRaisesRegex(ValueError, "executable permissions"):
            self.make_desktop("linux-x86_64", mode=0o644)
        with self.assertRaisesRegex(ValueError, "invalid desktop package entry"):
            self.make_desktop("linux-x86_64", extra=[("../escape", b"x", 0o644)])

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


class MacSigningTests(unittest.TestCase):
    """Signing and notarization commands; the services themselves run only in CI."""

    def test_developer_id_signs_for_notarization_and_ad_hoc_keeps_the_hardened_runtime(self):
        app, image, pem = Path("stage/image/Kureha Code.app"), Path("out/k.dmg"), Path("p/id.pem")
        self.assertEqual(release.macos_sign_command(app, pem),
                         ["rcodesign", "-C", "/dev/null", "sign", "--for-notarization",
                          "--pem-file", str(pem), str(app)])
        self.assertEqual(release.macos_sign_command(image, pem)[4:], ["--pem-file", str(pem), str(image)])
        self.assertEqual(release.macos_sign_command(app)[4:], ["--code-signature-flags", "runtime", str(app)])
        notary = release.notary_command(image, Path("key.json"))
        self.assertIn("--staple", notary)
        self.assertEqual(notary[notary.index("--api-key-file") + 1], "key.json")
        self.assertEqual(notary[-1], str(image))

    def test_certificates_in_either_pkcs12_encoding_are_converted(self):
        signing = release.MacSigning(Path("id.p12"), Path("password"), Path("key.json"))
        failed = unittest.mock.Mock(returncode=1, stderr="error:0308010C:unsupported RC2-40-CBC\n")
        converted = unittest.mock.Mock(returncode=0, stderr="")
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(release.subprocess, "run", side_effect=[failed, converted]) as run:
            pem = release.signing_pem(signing, Path(directory))
        self.assertEqual(pem.name, "developer-id.pem")
        first, second = (call.args[0] for call in run.call_args_list)
        self.assertNotIn("-legacy", first)
        self.assertEqual(second[:3], ["openssl", "pkcs12", "-legacy"])
        self.assertIn("file:password", second)  # never the password itself
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(release.subprocess, "run", return_value=failed):
            with self.assertRaisesRegex(ValueError, "cannot read the Developer ID certificate: .*RC2"):
                release.signing_pem(signing, Path(directory))

    def test_signing_credentials_go_together_and_only_for_macos(self):
        with self.assertRaises(SystemExit), patch("sys.stderr"):
            release.main(["build-desktop", "--name", "macos-aarch64", "--version", "local",
                          "--output", "unused", "--p12-file", "id.p12"])
        signing = release.MacSigning(Path("a"), Path("b"), Path("c"))
        with patch.object(release.sys, "platform", "linux"), \
                patch.object(release.host_platform, "machine", return_value="x86_64"):
            with self.assertRaisesRegex(ValueError, "only to macOS"):
                release.build_desktop("linux-x86_64", "local", Path("unused"), signing=signing)

    def test_apple_tools_run_without_the_nix_sdk(self):
        with patch.dict(release.os.environ, {"DEVELOPER_DIR": "/nix/store/sdk", "SDKROOT": "/nix/store/sdk/x"}), \
                patch.object(release.subprocess, "run", return_value=unittest.mock.Mock(stdout="out", stderr="")) as run:
            self.assertEqual(release.apple("xcrun", "stapler", "validate", Path("k.dmg"), capture=True), "out")
        command, env = run.call_args.args[0], run.call_args.kwargs["env"]
        self.assertEqual(command, ["/usr/bin/xcrun", "stapler", "validate", "k.dmg"])
        self.assertNotIn("DEVELOPER_DIR", env)
        self.assertNotIn("SDKROOT", env)


def udif_image(version=4):
    trailer = bytearray(512)
    trailer[:4] = b"koly"
    struct.pack_into(">II", trailer, 4, version, 512)
    return b"compressed image data" + bytes(trailer)


if __name__ == "__main__":
    unittest.main()
