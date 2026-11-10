#!/usr/bin/env python3
"""Build Kureha with Cargo and hand its reported executable to Appcoat."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
SCARLET_TARGETS = ("aarch64-unknown-scarlet", "riscv64gc-unknown-scarlet")


def build_artifact(root, target=None):
    command = ["cargo", "build", "--manifest-path", str(root / "Cargo.toml"),
               "--locked", "--release", "--bin", "kureha-code",
               "--message-format=json-render-diagnostics"]
    if target:
        command.extend(["--target", target])
    # Read Cargo's report instead of assuming target/release. This honors Cargo
    # config and CARGO_TARGET_DIR, including output paths containing spaces.
    artifacts = set()
    with subprocess.Popen(command, cwd=root, stdout=subprocess.PIPE, text=True) as process:
        for line in process.stdout:
            message = json.loads(line)
            if (message.get("reason") == "compiler-artifact"
                    and message.get("target", {}).get("name") == "kureha-code"
                    and "bin" in message.get("target", {}).get("kind", [])
                    and message.get("executable")):
                artifacts.add(Path(message["executable"]))
        if process.wait():
            raise RuntimeError("Cargo build failed; no package was created")
    if len(artifacts) != 1:
        raise RuntimeError("Cargo did not report exactly one Kureha executable")
    return artifacts.pop().resolve(strict=True)


def main(argv=None):
    host = {"darwin": "macos", "linux": "linux"}.get(sys.platform)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=("macos", "linux", "scarlet"), default=host)
    parser.add_argument("--target", choices=SCARLET_TARGETS, help="Required for Scarlet")
    parser.add_argument("--artifact", type=Path, help="Package an existing executable without building")
    parser.add_argument("--output", type=Path, help="New destination, relative to the current directory")
    args = parser.parse_args(argv)
    if args.platform is None:
        parser.error("select --platform and provide an existing --artifact on this host")
    if (args.platform == "scarlet") != bool(args.target):
        parser.error("--target is required only for --platform scarlet")
    if args.platform not in (host, "scarlet") and args.artifact is None:
        parser.error("cross-host packaging requires --artifact")
    if args.artifact is None and args.platform != "scarlet":
        configured_target = os.environ.get("CARGO_BUILD_TARGET")
        if configured_target and "scarlet" in configured_target:
            parser.error("unset CARGO_BUILD_TARGET when building a host application")
    appcoat = shutil.which("appcoat")
    if appcoat is None:
        parser.error("appcoat was not found; install Appcoat and add it to PATH, or run inside `nix develop`")
    with (ROOT / "appcoat.toml").open("rb") as stream:
        app = tomllib.load(stream)["app"]
    with (ROOT / "Cargo.toml").open("rb") as stream:
        version = tomllib.load(stream)["package"]["version"]
    if version != app["version"]:
        parser.error("appcoat.toml and Cargo.toml versions must match")
    name = {"macos": f'{app["name"]}.app', "linux": app["slug"],
            "scarlet": f'{app["slug"]}.app'}[args.platform]
    output = (args.output or ROOT / "dist" / args.platform / name).absolute()
    # Fail before an expensive build, including dangling symlinks.
    if os.path.lexists(output):
        parser.error(f"destination already exists: {output}; choose another --output")
    artifact = args.artifact.resolve(strict=True) if args.artifact else build_artifact(ROOT, args.target)
    command = [appcoat, "package", "--manifest", str(ROOT / "appcoat.toml"),
               "--platform", args.platform, "--artifact", str(artifact), "--output", str(output)]
    if args.target:
        command.extend(["--target", args.target])
    subprocess.run(command, check=True)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"package: {error}", file=sys.stderr)
        sys.exit(1)
