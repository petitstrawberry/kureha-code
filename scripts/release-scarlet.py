#!/usr/bin/env python3
"""Build Scarlet and desktop apps, assemble checksum-pinned bundles, and publish releases.

Scarlet archives and bundle.toml are what Scarlet images pin. Desktop packages
(macOS, Linux) ride in the same release and checksum file but never enter
bundle.toml, so adding them leaves the Scarlet side unchanged.
"""
import argparse
import gzip
import hashlib
import json
from pathlib import Path, PurePosixPath
import platform as host_platform
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "petitstrawberry/kureha-code"
TARGETS = {"aarch64-unknown-scarlet": ("aarch64", 183),
           "riscv64gc-unknown-scarlet": ("riscv64", 243)}
APP_PATH = "applications/kureha-code.app"
EXECUTABLE = APP_PATH + "/bin/kureha-code"
# Desktop builds: release name -> (Appcoat platform, architecture).
DESKTOP = {"macos-aarch64": ("macos", "aarch64"),
           "linux-x86_64": ("linux", "x86_64"),
           "linux-aarch64": ("linux", "aarch64")}
MACOS_APP = "Kureha Code.app"
MACOS_EXECUTABLE = MACOS_APP + "/Contents/MacOS/kureha-code"
LINUX_DIR = "kureha-code"
LINUX_EXECUTABLE = LINUX_DIR + "/bin/kureha-code"
# Linux binaries are built with Nix, then pointed at the distribution's loader
# with their Nix library search paths removed, so they run outside Nix.
LINUX_ELF = {"x86_64": (62, "/lib64/ld-linux-x86-64.so.2"),
             "aarch64": (183, "/lib/ld-linux-aarch64.so.1")}
MACHO_ARM64 = 0x0100000C
NIX_STORE = b"/nix/store"


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def version_tag(value):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", value):
        raise ValueError("invalid release tag")
    return value


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def verify_elf(header, target):
    if (len(header) < 64 or header[:8] != b"\x7fELF\x02\x01\x01\x53"
            or struct.unpack_from("<H", header, 16)[0] not in (2, 3)
            or struct.unpack_from("<H", header, 18)[0] != TARGETS[target][1]):
        raise ValueError("expected a native Scarlet ELF for " + target)


def verify_archive(path, target):
    """Check the image layout and target before composing or publishing a bundle."""
    names = set()
    descriptors = []
    with tarfile.open(path, "r:gz") as archive:
        for member in archive:
            name = member.name
            relative = PurePosixPath(name)
            if (relative.is_absolute() or ".." in relative.parts
                    or str(relative) != name or name in names
                    or not name.startswith(APP_PATH + "/") and name != APP_PATH
                    or not (member.isfile() or member.isdir())):
                raise ValueError("invalid app archive member: " + name)
            names.add(name)
            if member.isfile():
                with archive.extractfile(member) as stream:
                    header = stream.read(64)
                if header.startswith(b"\x7fELF"):
                    verify_elf(header, target)
                if name == EXECUTABLE:
                    verify_elf(header, target)
                    if not member.mode & 0o111:
                        raise ValueError("app executable lacks executable permissions")
                if relative.parent == PurePosixPath(APP_PATH) and name.endswith(".desktop"):
                    descriptors.append(name)
        if EXECUTABLE not in names or len(descriptors) != 1:
            raise ValueError("archive must contain the app executable and one descriptor")
        with archive.extractfile(descriptors[0]) as stream:
            descriptor = stream.read().decode()
        fields = dict(line.split("=", 1) for line in descriptor.splitlines()
                      if "=" in line and not line.startswith("#"))
        if (fields.get("X-Scarlet-AppFormat") != "1"
                or fields.get("X-Scarlet-Target") != target
                or fields.get("Exec") != "bin/kureha-code %f"):
            raise ValueError("invalid Scarlet app descriptor")


def write_tar(stage, top, archive_path):
    """Write `top` (inside `stage`) with normalized metadata and gzip headers."""
    with archive_path.open("wb") as stream:
        with gzip.GzipFile(fileobj=stream, mode="wb", filename="", mtime=0) as gz:
            with tarfile.open(fileobj=gz, mode="w") as archive:
                for path in [top, *sorted(top.rglob("*"))]:
                    if path.is_symlink() or not (path.is_dir() or path.is_file()):
                        raise ValueError("unsupported app file: " + str(path))
                    info = archive.gettarinfo(str(path), arcname=path.relative_to(stage).as_posix())
                    info.uid = info.gid = info.mtime = 0
                    info.uname = info.gname = ""
                    info.mode = 0o755 if path.is_dir() or path.stat().st_mode & 0o111 else 0o644
                    if path.is_file():
                        with path.open("rb") as payload:
                            archive.addfile(info, payload)
                    else:
                        archive.addfile(info)


def linux_executable(data, arch):
    """Check a desktop Linux ELF and return the newest glibc version it needs.

    The checks are structural: the loader, the absence of RPATH/RUNPATH and
    bare library names. Nix store strings may remain as unused bytes, such as
    a removed RUNPATH or a library's last-resort search directory.
    """
    machine, loader = LINUX_ELF[arch]
    if (len(data) < 64 or data[:7] != b"\x7fELF\x02\x01\x01" or data[7] not in (0, 3)
            or struct.unpack_from("<H", data, 18)[0] != machine):
        raise ValueError(f"expected a {arch} Linux ELF")
    phoff, = struct.unpack_from("<Q", data, 32)
    phentsize, phnum = struct.unpack_from("<HH", data, 54)
    interpreter, dynamic, loads = None, None, []
    for index in range(phnum):
        offset = phoff + index * phentsize
        if offset + 56 > len(data):
            raise ValueError("truncated ELF program headers")
        kind, = struct.unpack_from("<I", data, offset)
        p_offset, p_vaddr = struct.unpack_from("<QQ", data, offset + 8)
        p_filesz, = struct.unpack_from("<Q", data, offset + 32)
        if kind == 3:  # PT_INTERP
            interpreter = data[p_offset:p_offset + p_filesz].rstrip(b"\0").decode(errors="replace")
        elif kind == 2:  # PT_DYNAMIC
            dynamic = (p_offset, p_filesz)
        elif kind == 1:  # PT_LOAD
            loads.append((p_vaddr, p_offset, p_filesz))
    if interpreter != loader:
        raise ValueError(f"Linux executable uses loader {interpreter!r}, expected {loader}")
    entries = []
    if dynamic:
        start, size = dynamic
        for at in range(start, min(start + size, len(data)) - 15, 16):
            tag, value = struct.unpack_from("<qQ", data, at)
            if tag == 0:  # DT_NULL
                break
            entries.append((tag, value))
    if any(tag in (15, 29) for tag, _ in entries):  # DT_RPATH, DT_RUNPATH
        raise ValueError("Linux executable still has a library search path")
    strtab = next((value for tag, value in entries if tag == 5), None)  # DT_STRTAB
    base = next((offset + strtab - vaddr for vaddr, offset, size in loads
                 if strtab is not None and vaddr <= strtab < vaddr + size), None)
    for tag, value in entries:
        if tag == 1:  # DT_NEEDED
            if base is None:
                raise ValueError("Linux executable has an unreadable string table")
            name = data[base + value:data.index(b"\0", base + value)].decode(errors="replace")
            if "/" in name:
                raise ValueError("Linux executable needs a library by path: " + name)
    versions = [tuple(map(int, v)) for v in re.findall(rb"GLIBC_(\d+)\.(\d+)", data)]
    return "%d.%d" % max(versions) if versions else None


def macos_executable(data):
    if len(data) < 8 or data[:4] != b"\xcf\xfa\xed\xfe" \
            or struct.unpack_from("<I", data, 4)[0] != MACHO_ARM64:
        raise ValueError("expected an arm64 Mach-O executable")


def verify_desktop_archive(path, name):
    """Check layout, architecture and the absence of Nix store paths."""
    platform, arch = DESKTOP[name]
    top, executable = (MACOS_APP, MACOS_EXECUTABLE) if platform == "macos" else (LINUX_DIR, LINUX_EXECUTABLE)
    files = {}
    if platform == "macos":
        with zipfile.ZipFile(path) as archive:
            for info in archive.infolist():
                files[info.filename.rstrip("/")] = (
                    info.is_dir(), (info.external_attr >> 16) & 0o777,
                    None if info.is_dir() else archive.read(info))
    else:
        with tarfile.open(path, "r:gz") as archive:
            for member in archive:
                if not (member.isfile() or member.isdir()):
                    raise ValueError("invalid desktop archive member: " + member.name)
                data = archive.extractfile(member).read() if member.isfile() else None
                files[member.name] = (member.isdir(), member.mode, data)
    for entry, (_, _, data) in files.items():
        relative = PurePosixPath(entry)
        if (relative.is_absolute() or ".." in relative.parts or str(relative) != entry
                or not (entry == top or entry.startswith(top + "/"))):
            raise ValueError("invalid desktop archive member: " + entry)
        # The Linux executable is checked structurally instead (see linux_executable).
        linux_binary = platform == "linux" and entry == executable
        if data is not None and NIX_STORE in data and not linux_binary:
            raise ValueError("desktop package refers to the Nix store: " + entry)
    if executable not in files or files[executable][2] is None:
        raise ValueError("desktop archive lacks " + executable)
    _, mode, data = files[executable]
    if not mode & 0o111:
        raise ValueError("desktop executable lacks executable permissions")
    if platform == "macos":
        macos_executable(data)
        return None
    return linux_executable(data, arch)


def desktop_archive_name(name, version):
    return f"kureha-code-{name}-{version}." + ("zip" if DESKTOP[name][0] == "macos" else "tar.gz")


def build_desktop(name, version, output, artifact=None):
    """Package a desktop build for this host inside `nix develop`; no cross builds."""
    version_tag(version)
    platform, arch = DESKTOP[name]
    host = ({"darwin": "macos", "linux": "linux"}.get(sys.platform),
            {"arm64": "aarch64", "amd64": "x86_64"}.get(host_platform.machine().lower(),
                                                          host_platform.machine().lower()))
    if host != (platform, arch):
        raise ValueError(f"{name} must be built on {platform}/{arch}, not {host[0]}/{host[1]}")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True))
    output.mkdir(parents=True, exist_ok=False)
    try:
        archive_path = output / desktop_archive_name(name, version)
        with tempfile.TemporaryDirectory(prefix="kureha-desktop-") as temporary:
            stage = Path(temporary)
            top = stage / (MACOS_APP if platform == "macos" else LINUX_DIR)
            command = [sys.executable, str(ROOT / "scripts/package.py"), "--platform", platform,
                       "--output", str(top)]
            if artifact:
                command.extend(["--artifact", str(artifact.resolve(strict=True))])
            subprocess.run(command, cwd=ROOT, check=True)
            if platform == "macos":
                subprocess.run(["codesign", "--verify", "--deep", "--strict", str(top)], check=True)
                for path in sorted(top.rglob("*")):
                    if path.is_file() and path.read_bytes()[:4] == b"\xcf\xfa\xed\xfe":
                        links = subprocess.check_output(["otool", "-L", str(path)], text=True)
                        if "/nix/store" in links:
                            raise ValueError("macOS package links the Nix store: " + path.name)
                # No resource forks or extended attributes: the zip holds the bundle only.
                subprocess.run(["ditto", "-c", "-k", "--norsrc", "--noextattr", "--noacl",
                                "--keepParent", str(top), str(archive_path)], check=True)
            else:
                # Run on the distribution's loader and libraries, not Nix's.
                subprocess.run(["patchelf", "--set-interpreter", LINUX_ELF[arch][1], "--remove-rpath",
                                str(stage / LINUX_EXECUTABLE)], check=True)
                write_tar(stage, top, archive_path)
        glibc = verify_desktop_archive(archive_path, name)
        record = {
            "version": version, "name": name, "platform": platform, "arch": arch,
            "revision": revision, "dirty": dirty, "external_artifact": artifact is not None,
            "archive": archive_path.name, "sha256": sha(archive_path),
            "cargo_lock_sha256": sha(ROOT / "Cargo.lock"),
            "flake_lock_sha256": sha(ROOT / "flake.lock"),
        }
        if glibc:
            record["glibc"] = glibc
        write_json(output / f"desktop-{name}.json", record)
    except BaseException:
        shutil.rmtree(output)
        raise


def build(target, version, output, artifact=None):
    version_tag(version)
    arch = TARGETS[target][0]
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True))
    output.mkdir(parents=True, exist_ok=False)
    try:
        archive_path = output / f"kureha-code-{arch}-{version}.tar.gz"
        with tempfile.TemporaryDirectory(prefix="kureha-scarlet-") as temporary:
            stage = Path(temporary)
            app = stage / APP_PATH
            command = [sys.executable, str(ROOT / "scripts/package.py"), "--platform", "scarlet",
                       "--target", target, "--output", str(app)]
            if artifact:
                command.extend(["--artifact", str(artifact.resolve(strict=True))])
            subprocess.run(command, cwd=ROOT, check=True)
            # Include the complete .app, with Appcoat resources and notices,
            # under its runtime path.
            write_tar(stage, app, archive_path)
        verify_archive(archive_path, target)
        write_json(output / f"{arch}.json", {
            "version": version, "target": target, "arch": arch,
            "revision": revision, "dirty": dirty, "external_artifact": artifact is not None,
            "archive": archive_path.name, "sha256": sha(archive_path),
            "cargo_lock_sha256": sha(ROOT / "Cargo.lock"),
            "flake_lock_sha256": sha(ROOT / "flake.lock"),
        })
    except BaseException:
        shutil.rmtree(output)
        raise


def assemble(inputs, version, output, desktop=False):
    version_tag(version)
    records = []
    for target, (arch, _) in TARGETS.items():
        record = json.loads((inputs / f"{arch}.json").read_text())
        name = f"kureha-code-{arch}-{version}.tar.gz"
        if (record["target"] != target or record["arch"] != arch or record["version"] != version
                or record["archive"] != name or record["sha256"] != sha(inputs / name)):
            raise ValueError("release input mismatch: " + arch)
        verify_archive(inputs / name, target)
        records.append(record)
    desktop_records = []
    for name, (platform, arch) in DESKTOP.items() if desktop else ():
        record = json.loads((inputs / f"desktop-{name}.json").read_text())
        archive = desktop_archive_name(name, version)
        if (record["name"] != name or record["platform"] != platform or record["arch"] != arch
                or record["version"] != version or record["archive"] != archive
                or record["sha256"] != sha(inputs / archive)):
            raise ValueError("release input mismatch: " + name)
        if verify_desktop_archive(inputs / archive, name) != record.get("glibc"):
            raise ValueError("release input mismatch: " + name)
        desktop_records.append(record)
    for key in ("revision", "cargo_lock_sha256", "flake_lock_sha256"):
        if len({record[key] for record in records + desktop_records}) != 1:
            raise ValueError("architecture builds disagree on " + key)
    output.mkdir(parents=True, exist_ok=False)
    for record in records + desktop_records:
        shutil.copyfile(inputs / record["archive"], output / record["archive"])
    checksums = ", ".join(f'{record["arch"]} = "sha256:{record["sha256"]}"' for record in records)
    url = f"https://github.com/{REPOSITORY}/releases/download/{version}/kureha-code-{{arch}}-{version}.tar.gz"
    (output / "bundle.toml").write_text(
        "# Generated from verified Scarlet release archives.\n"
        f"# Source revision: {records[0]['revision']}\n"
        '[[layers]]\nkind = "archive"\n'
        f'url = "{url}"\nsha256 = {{ {checksums} }}\n'
        'format = "tar-gz"\nto = "/"\n')
    manifest = {"version": version, "builds": records}
    if desktop:
        manifest["desktop"] = desktop_records
    write_json(output / "release.json", manifest)
    (output / "SHA256SUMS").write_text("".join(
        f"{sha(path)}  {path.name}\n" for path in sorted(output.iterdir())))


def verify_release(directory):
    release = json.loads((directory / "release.json").read_text())
    version = version_tag(release["version"])
    with tempfile.TemporaryDirectory(prefix="kureha-release-verify-") as temporary:
        inputs = Path(temporary) / "inputs"
        inputs.mkdir()
        for record in release["builds"]:
            if record["target"] not in TARGETS or record["arch"] != TARGETS[record["target"]][0]:
                raise ValueError("invalid release target")
            name = f"kureha-code-{record['arch']}-{version}.tar.gz"
            if record["archive"] != name:
                raise ValueError("invalid archive name")
            shutil.copyfile(directory / name, inputs / name)
            write_json(inputs / f"{record['arch']}.json", record)
        desktop = "desktop" in release
        for record in release.get("desktop", []):
            if record.get("name") not in DESKTOP:
                raise ValueError("invalid desktop build")
            name = desktop_archive_name(record["name"], version)
            if record["archive"] != name:
                raise ValueError("invalid archive name")
            shutil.copyfile(directory / name, inputs / name)
            write_json(inputs / f"desktop-{record['name']}.json", record)
        expected = Path(temporary) / "expected"
        assemble(inputs, version, expected, desktop)
        actual_files = {path.name for path in directory.iterdir()}
        if actual_files != {path.name for path in expected.iterdir()}:
            raise ValueError("unexpected release files")
        for path in expected.iterdir():
            if sha(path) != sha(directory / path.name):
                raise ValueError("release verification failed: " + path.name)
    return release


def release_notes(release):
    scarlet = ("**Scarlet**: native AArch64 and RISC-V64 apps and a checksum-pinned `bundle.toml`. "
               "Requires Scarlet's SWS and installed SGFX driver. Shared graphics libraries are provided by the OS.")
    if "desktop" not in release:
        return scarlet
    glibc = max((tuple(map(int, r["glibc"].split("."))) for r in release["desktop"] if r.get("glibc")),
                default=None)
    linux_glibc = f" glibc {glibc[0]}.{glibc[1]} or newer." if glibc else ""
    return "\n\n".join([
        scarlet,
        "**macOS** (Apple silicon): `kureha-code-macos-aarch64-*.zip`. The app is ad-hoc signed, "
        "not notarized: after unzipping, run `xattr -dr com.apple.quarantine \"Kureha Code.app\"` "
        "or allow it in System Settings > Privacy & Security.",
        "**Linux** (x86_64, AArch64): `kureha-code-linux-*.tar.gz`; run `kureha-code/bin/kureha-code`." + linux_glibc +
        " Install Fontconfig and the graphics stack from your distribution: Vulkan or OpenGL drivers, "
        "plus Wayland/xkbcommon or X11 libraries.",
        "Verify downloads with `SHA256SUMS`.",
    ])


def publish(directory, prerelease):
    release = verify_release(directory)
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if any(record["revision"] != revision or record["dirty"] or record["external_artifact"]
           for record in release["builds"] + release.get("desktop", [])):
        raise ValueError("publish requires clean source builds matching the checkout")
    version = release["version"]
    command = ["gh", "release", "create", version, "--repo", REPOSITORY, "--target", revision,
               "--title", ("Kureha Code " if "desktop" in release else "Kureha Code for Scarlet ") + version,
               "--notes", release_notes(release)]
    if prerelease:
        command.append("--prerelease")
    # gh release create fails if this tag already has a release. Never replace assets.
    subprocess.run([*command, *[str(path) for path in sorted(directory.iterdir())]], check=True)
    with tempfile.TemporaryDirectory(prefix="kureha-release-download-") as temporary:
        downloaded = Path(temporary)
        subprocess.run(["gh", "release", "download", version, "--repo", REPOSITORY,
                        "--dir", str(downloaded)], check=True)
        verify_release(downloaded)
        for path in directory.iterdir():
            if sha(path) != sha(downloaded / path.name):
                raise ValueError("published asset differs: " + path.name)
    # The workflow commits only this file after all uploaded assets are verified.
    bundle = ROOT / "platforms/scarlet/bundle.toml"
    bundle.parent.mkdir(parents=True, exist_ok=True)
    bundle.write_bytes((directory / "bundle.toml").read_bytes())


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    builder = commands.add_parser("build")
    builder.add_argument("--target", choices=TARGETS, required=True)
    builder.add_argument("--version", required=True)
    builder.add_argument("--output", type=Path, required=True)
    builder.add_argument("--artifact", type=Path, help="Package an existing binary for local validation only")
    desktop = commands.add_parser("build-desktop")
    desktop.add_argument("--name", choices=DESKTOP, required=True)
    desktop.add_argument("--version", required=True)
    desktop.add_argument("--output", type=Path, required=True)
    desktop.add_argument("--artifact", type=Path, help="Package an existing binary for local validation only")
    assembler = commands.add_parser("assemble")
    assembler.add_argument("--inputs", type=Path, required=True)
    assembler.add_argument("--version", required=True)
    assembler.add_argument("--output", type=Path, required=True)
    assembler.add_argument("--desktop", action="store_true", help="Require and include the desktop builds")
    verifier = commands.add_parser("verify")
    verifier.add_argument("--artifacts", type=Path, required=True)
    publisher = commands.add_parser("publish")
    publisher.add_argument("--artifacts", type=Path, required=True)
    publisher.add_argument("--prerelease", action="store_true")
    args = parser.parse_args(argv)
    if args.command == "build":
        build(args.target, args.version, args.output.resolve(), args.artifact)
    elif args.command == "build-desktop":
        build_desktop(args.name, args.version, args.output.resolve(), args.artifact)
    elif args.command == "assemble":
        assemble(args.inputs.resolve(), args.version, args.output.resolve(), args.desktop)
    elif args.command == "verify":
        verify_release(args.artifacts.resolve())
    else:
        publish(args.artifacts.resolve(), args.prerelease)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError, tarfile.TarError,
            zipfile.BadZipFile) as error:
        print(f"scarlet-release: {error}", file=sys.stderr)
        sys.exit(1)
