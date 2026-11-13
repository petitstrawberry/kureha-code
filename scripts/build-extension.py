#!/usr/bin/env python3
"""Build Kureha Code extensions into folders that install as they are.

An extension's source is a Cargo project beside its `extension.toml`. The
manifest describes the installed folder: `[extension] wasm` names the module
there (`extension.wasm`). This builds the module for wasm32 with Cargo and
puts it, the manifest, the files the manifest refers to (themes), and README
and LICENSE files into `<out>/<id>/`; sources and Cargo files stay behind.

    scripts/build-extension.py --install examples/extensions/direnv
    scripts/build-extension.py --out dist/extensions        # every example

The new folder replaces an earlier install only once it is complete, so a
failed build leaves the old one working. A symbolic link that held the same
extension (a source folder linked in by hand) is replaced; another real folder
with the same id is an error, since Kureha would load only one of them.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
EXAMPLES = ROOT / "examples/extensions"
TARGET = "wasm32-unknown-unknown"
MANIFEST = "extension.toml"
# Copied when present beside the manifest: README.md, LICENSE-MIT and so on.
DOCUMENTS = ("README", "LICENSE", "COPYING", "NOTICE")


class BuildError(Exception):
    pass


def kureha_extensions_dir():
    """Where Kureha Code loads extensions from (see `extensions_dir`)."""
    if os.environ.get("KUREHA_EXTENSIONS_DIR"):
        return Path(os.environ["KUREHA_EXTENSIONS_DIR"])
    config = os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")
    return Path(config) / "kureha-code/extensions"


def read_manifest(folder):
    try:
        return tomllib.loads((folder / MANIFEST).read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise BuildError(f"{folder / MANIFEST}: {error}") from error


def manifest_id(folder):
    """The id in `folder`'s manifest, or None if it has none it can read."""
    try:
        return tomllib.loads((folder / MANIFEST).read_text(encoding="utf-8"))["extension"]["id"]
    except (OSError, tomllib.TOMLDecodeError, KeyError, TypeError):
        return None


def inside(path, what):
    """`path` as a relative path that stays inside the extension folder."""
    relative = Path(path) if isinstance(path, str) else None
    if relative is None or relative.is_absolute() or not relative.parts or ".." in relative.parts:
        raise BuildError(f"{what} must be a path inside the extension folder, not {path!r}")
    return relative


def build_module(folder):
    """Builds `folder`'s Cargo project for wasm32 and returns its module, as
    Cargo reports it (this honours CARGO_TARGET_DIR and Cargo config)."""
    cargo_toml = (folder / "Cargo.toml").resolve()
    if not cargo_toml.is_file():
        raise BuildError(f"{folder}: the manifest names a wasm module but there is no Cargo.toml")
    command = ["cargo", "build", "--manifest-path", str(cargo_toml), "--locked", "--release",
               "--target", TARGET, "--message-format=json-render-diagnostics"]
    modules = set()
    with subprocess.Popen(command, cwd=folder, stdout=subprocess.PIPE, text=True) as process:
        for line in process.stdout:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            # Only this crate's cdylib: path dependencies (the SDK) report too.
            if (message.get("reason") == "compiler-artifact"
                    and Path(message.get("manifest_path", "")).resolve() == cargo_toml
                    and "cdylib" in message.get("target", {}).get("kind", [])):
                modules.update(Path(name) for name in message.get("filenames", [])
                               if name.endswith(".wasm"))
        if process.wait():
            raise BuildError(f"{folder.name}: Cargo build failed")
    if len(modules) != 1:
        raise BuildError(f"{folder.name}: Cargo did not report exactly one wasm module "
                         "(is the crate a cdylib?)")
    return modules.pop()


def install(source, out):
    """Builds the extension in `source` into `out/<id>/` and returns that."""
    source = source.resolve()
    manifest = read_manifest(source)
    extension = manifest.get("extension", {})
    id = extension.get("id")
    if not isinstance(id, str) or not id or id.startswith(".") or "/" in id or "\\" in id:
        raise BuildError(f"{source / MANIFEST}: [extension] id must be a plain name")
    files = {Path(MANIFEST)}
    for theme in manifest.get("contributes", {}).get("themes", []):
        files.add(inside(theme.get("path"), "[[contributes.themes]] path"))
    files.update(Path(entry.name) for entry in source.iterdir()
                 if entry.is_file() and entry.name.upper().startswith(DOCUMENTS))
    for path in files:
        if not (source / path).is_file():
            raise BuildError(f"{id}: {path} is missing from {source}")
    wasm = extension.get("wasm")
    if wasm is not None:
        wasm = inside(wasm, "[extension] wasm")
        files.discard(wasm)

    out.mkdir(parents=True, exist_ok=True)
    destination = out / id
    if destination.exists() and not destination.is_symlink():
        if not destination.is_dir() or manifest_id(destination) not in (id, None):
            raise BuildError(f"{destination} holds something else; move it away first")
    # Kureha loads one folder per id, so any other holder must go. Links (a
    # source folder linked in by hand) are replaced; real folders are not.
    others = [entry for entry in out.iterdir()
              if entry.name != id and not entry.name.startswith(".") and entry.is_dir()
              and manifest_id(entry) == id]
    for other in others:
        if not other.is_symlink():
            raise BuildError(f"{other} also holds {id}; remove it first")

    module = build_module(source) if wasm is not None else None
    staging = Path(tempfile.mkdtemp(prefix=f".{id}.", dir=out))
    # mkdtemp makes it private; give it the permissions of any new folder.
    umask = os.umask(0)
    os.umask(umask)
    staging.chmod(0o777 & ~umask)
    try:
        for path in sorted(files):
            (staging / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source / path, staging / path)
        if module is not None:
            (staging / wasm).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(module, staging / wasm)
        previous = None
        if destination.is_symlink():
            destination.unlink()
        elif destination.exists():
            previous = Path(tempfile.mkdtemp(prefix=f".{id}.old.", dir=out))
            destination.rename(previous / "extension")
        staging.rename(destination)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    if previous is not None:
        shutil.rmtree(previous, ignore_errors=True)
    for other in others:
        print(f"Removed {other} (a link to {os.readlink(other)}), which also held {id}")
        other.unlink()
    return destination


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Build Kureha Code extensions into folders that install as they are.")
    parser.add_argument("extensions", nargs="*", type=Path,
                        help="extension source folders (default: every example)")
    where = parser.add_mutually_exclusive_group()
    where.add_argument("--out", type=Path, help="folder to put the extensions in "
                       "(default: dist/extensions)")
    where.add_argument("--install", action="store_true",
                       help="put them where Kureha Code loads extensions from")
    args = parser.parse_args(argv)
    out = kureha_extensions_dir() if args.install else args.out or ROOT / "dist/extensions"
    sources = args.extensions or sorted(
        folder for folder in EXAMPLES.iterdir() if (folder / MANIFEST).is_file())
    try:
        for source in sources:
            print(install(source, out))
    except (BuildError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    if args.install:
        print("Restart Kureha Code to load them.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
