# Kureha Code

A native, cross-platform code editor written in pure Rust with ScarletUI.

![Markdown source and preview](docs/screenshots/markdown-preview.png)

## Build and run

With Nix and flakes enabled:

```sh
nix develop
cargo run -- path/to/file-or-directory
```

The development shell supports Linux x86_64/AArch64 and macOS Apple Silicon.
With direnv, run `direnv allow` to load it automatically.

## Package

Build a release package with Nix (no development shell required):

```sh
nix build
```

On macOS, open `result/Applications/Kureha Code.app` or copy it to Applications.
On Linux, run `result/bin/kureha-code`; its wrapper supplies the Nix runtime
libraries and a default font configuration. For desktop launchers, install the
flake with `nix profile add .#kureha-code`, or add `result/bin` to `PATH` and
`result/share` to `XDG_DATA_DIRS`. The build uses the pinned Rust toolchain,
vendors the locked Cargo dependencies and packages the executable, icons and
resources with [Appcoat](https://github.com/petitstrawberry/appcoat).
macOS packages are not distribution-signed or notarized.

The packaging helper also works without Nix. Install Rust/Cargo, Python 3.11 or
newer, and [Appcoat](https://github.com/petitstrawberry/appcoat), with `cargo` and
`appcoat` on `PATH`. To install the same Appcoat revision used by the flake:

```sh
cargo install --git https://github.com/petitstrawberry/appcoat \
  --rev 3ddff8882449d82ce4c8829d538b0157b5ff0096 --locked appcoat_cli
python3 scripts/package.py
```

The script builds a release executable with Cargo and packages it with Appcoat.
macOS packaging requires Apple's command-line tools. On Linux, install
`pkg-config`, Fontconfig development files, native runtime libraries and graphics
drivers separately; the helper does not bundle runtime libraries.
The Nix development shell provides the tools and Linux runtime libraries
as another way to run the same helper:

```sh
nix develop --command python3 scripts/package.py
```

Use `--artifact /path/to/kureha-code` to package an existing binary, or
`--output /path/to/new-package` to choose another destination. Existing packages
are never overwritten. This helper writes to `dist/macos/Kureha Code.app` or
`dist/linux/kureha-code` by default. On macOS, open the generated `.app` or copy it
to Applications. On Linux, run `bin/kureha-code` inside the generated directory;
desktop launchers need its `bin` on `PATH` and `share` on `XDG_DATA_DIRS`.

## Releases

Each release carries the Scarlet apps and desktop packages for macOS and Linux.

| Platform | Asset | Notes |
| --- | --- | --- |
| macOS (Apple silicon) | `kureha-code-macos-aarch64-<tag>.zip` | Ad-hoc signed, not notarized: after unzipping, run `xattr -dr com.apple.quarantine "Kureha Code.app"` or allow it in System Settings → Privacy & Security |
| Linux x86_64 / AArch64 | `kureha-code-linux-<arch>-<tag>.tar.gz` | Run `kureha-code/bin/kureha-code`. Needs the glibc version in the release notes, Fontconfig, Vulkan or OpenGL drivers and Wayland/xkbcommon or X11 libraries from the distribution |
| Scarlet | see below | Installed into images through `bundle.toml` |

Desktop packages are built outside Nix on GitHub-hosted runners (macOS 15,
Ubuntu 22.04) with a pinned stable Rust and the flake's Appcoat revision, so
they use the system loader and libraries. The build refuses packages that refer
to `/nix/store`, Linux binaries with another dynamic loader, wrong architectures
and macOS bundles whose signature does not verify. `release.json` records each
build's source revision, and Linux builds also record the newest glibc symbol
version they need. Desktop assets are listed in `SHA256SUMS` but never in
`bundle.toml`, so Scarlet images are unaffected.

```sh
python3 scripts/release-scarlet.py build-desktop --name linux-x86_64 \
  --version local --output artifacts/desktop
```

Names are `macos-aarch64`, `linux-x86_64` and `linux-aarch64`; each must be built
on that platform. `assemble --desktop` requires all three.

## Scarlet releases

The Scarlet CD workflow uses the pinned toolchain and Appcoat from this flake.
It builds `aarch64-unknown-scarlet` and `riscv64gc-unknown-scarlet` with Cargo,
then packages each complete `.app` into a separate `.tar.gz`. Image composition
downloads and extracts these archives; it does not build the application.
The graphics driver and shared runtime libraries are supplied by Scarlet.

Each release includes:

- `kureha-code-aarch64-<tag>.tar.gz` and `kureha-code-riscv64-<tag>.tar.gz`
- `bundle.toml`, with release URLs and an architecture-keyed SHA-256 table
- `release.json`, recording the source revision and Cargo/Nix lockfile hashes
- `SHA256SUMS`, covering both archives, the desktop packages and the manifests

Changes on `main` automatically publish a uniquely named prerelease.
Pushing a `v*` tag publishes a regular release. Pull requests build and verify
all Scarlet and desktop targets without publishing. The **Build and release apps**
workflow also supports manual runs with an optional fresh tag and a `publish`
switch. Existing release assets are never overwritten.

After publishing, CD downloads and verifies the assets again. For `main`
builds it then commits the generated `platforms/scarlet/bundle.toml`; this file
is created by the first successful publication. Bundle-only updates do not
trigger another build. Consumers pin that bundle commit:

```toml
[[images.rootfs.layers]]
kind = "bundle"
source = { git = "https://github.com/petitstrawberry/kureha-code.git", rev = "<published-bundle-commit>" }
bundle = "platforms/scarlet/bundle.toml"
```

Alternatively, download the release's `bundle.toml`, commit it to the consuming
project, and reference it with a local bundle layer. Both ways install the app
at `/applications/kureha-code.app` using only `kind = "archive"`.

To reproduce the CD build locally, choose a new output directory for each step:

```sh
nix develop --command python3 scripts/release-scarlet.py build \
  --target aarch64-unknown-scarlet --version scarlet-local --output artifacts/aarch64
nix develop --command python3 scripts/release-scarlet.py build \
  --target riscv64gc-unknown-scarlet --version scarlet-local --output artifacts/riscv64
mkdir artifacts/inputs
cp artifacts/aarch64/* artifacts/riscv64/* artifacts/inputs/
python3 scripts/release-scarlet.py assemble \
  --inputs artifacts/inputs --version scarlet-local --output artifacts/release
python3 scripts/release-scarlet.py verify --artifacts artifacts/release
```

For local packaging of an existing executable, the original helper remains available:

```sh
python3 scripts/package.py --platform scarlet --target aarch64-unknown-scarlet \
  --artifact /path/to/scarlet/kureha-code
```

`riscv64gc-unknown-scarlet` is also accepted. This writes a local `.app` directory;
the release tooling creates the archive and bundle used for image composition.
`release-scarlet.py build --artifact` can exercise packaging with a prebuilt
binary, but such builds cannot be published by the release command.

## Configuration

Open the Settings tab with **Kureha Code → Settings…** (`⌘,` on macOS, `Ctrl+,` elsewhere).
Theme choices, code font size and minimap visibility are saved automatically.
Settings also includes the bundled dependency, font and theme acknowledgements.

**Open Settings File…** opens `~/.config/kureha-code/settings.toml`.
**Edit Custom Theme…** opens `~/.config/kureha-code/theme.toml`;
see the [configuration example](assets/theme.toml) for available overrides.

Language servers must be installed separately. Override their commands in
`~/.config/kureha-code/lsp.toml`:

```toml
[servers.rust]
command = "/path/to/rust-analyzer"
args = []
```

Configuration paths respect `$XDG_CONFIG_HOME`.

## Development

Run tests inside the development shell:

```sh
cargo test --lib --bin kureha-code
```
