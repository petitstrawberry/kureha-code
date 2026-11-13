{
  description = "Kureha Code packages and development shell";

  nixConfig = {
    extra-substituters = [ "https://scarlet-rust-toolchain.cachix.org" ];
    extra-trusted-public-keys = [
      "scarlet-rust-toolchain.cachix.org-1:p+coBExi0nNTIvWF/oM9H9/1/GhwFtqGZ2Vs+4pYl6o="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/3e41b24abd260e8f71dbe2f5737d24122f972158";
    appcoat = {
      url = "github:petitstrawberry/appcoat/3ddff8882449d82ce4c8829d538b0157b5ff0096";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    scarlet-rust-toolchain.url = "github:petitstrawberry/scarlet-rust-nix/815e2e998de8ef814130c73af3ee2a5e63168e15";
  };

  outputs = { nixpkgs, scarlet-rust-toolchain, appcoat, ... }:
    let
      # These are the host platforms provided by the pinned Scarlet toolchain.
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      runtimeLibraries = pkgs: with pkgs; lib.optionals stdenv.isLinux [
        fontconfig
        freetype
        vulkan-loader
        mesa
        libxkbcommon
        wayland
        libGL
        libx11
        libxcursor
        libxi
        libxrandr
        libxcb
        stdenv.cc.cc.lib
      ];
      package = system:
        let
          pkgs = import nixpkgs { inherit system; };
          rust = scarlet-rust-toolchain.packages.${system}.scarlet-rust-toolchain;
          rustPlatform = pkgs.makeRustPlatform { cargo = rust; rustc = rust; };
          upstreamAppcoat = appcoat.packages.${system}.appcoat;
          packager = if pkgs.stdenv.isDarwin then upstreamAppcoat.overrideAttrs (old: {
            postPatch = (old.postPatch or "") + ''
              substituteInPlace crates/appcoat_build/src/macos.rs \
                --replace-fail /usr/bin/otool ${pkgs.darwin.cctools}/bin/otool \
                --replace-fail /usr/bin/install_name_tool ${pkgs.darwin.cctools}/bin/install_name_tool
              # Defer signing to rcodesign below; Apple's host codesign is not
              # available in the Nix sandbox. Keep Appcoat's dylib relocation.
              ${pkgs.python3}/bin/python3 ${pkgs.writeText "appcoat-nix-signing.py" ''
                from pathlib import Path
                path = Path("crates/appcoat_build/src/macos.rs")
                source = path.read_text()
                start = source.index("    // Changes to load commands invalidate existing signatures.")
                end = source.index("    Ok(())", start)
                path.write_text(source[:start] + source[end:])
              ''}
            '';
          }) else upstreamAppcoat;
          manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          app = (builtins.fromTOML (builtins.readFile ./appcoat.toml)).app;
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml ./Cargo.lock ./build.rs ./src ./assets ./tests
              ./appcoat.toml ./LICENSE
              # Existing editor tests open these repository fixtures.
              ./README.md ./docs/screenshots/panel-bottom.png
            ];
          };
          fontConfig = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
          binary = rustPlatform.buildRustPackage {
            pname = "kureha-code-unwrapped";
            version = manifest.package.version;
            inherit src;
            cargoHash = "sha256-5bNP+KlIj4xL7bt/zPPPHGAG/Fo909d96dboEbYyJG4=";
            nativeBuildInputs = [ pkgs.pkg-config ];
            # Git tests build their fixture repositories with the git command line.
            nativeCheckInputs = [ pkgs.python3 pkgs.git ];
            # Transport and Markdown tests use local loopback servers.
            __darwinAllowLocalNetworking = true;
            buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.fontconfig ];
            postPatch = ''
              # The Linux sandbox has no host /bin/kill.
              substituteInPlace src/lsp_tests.rs \
                --replace-fail 'Command::new("/bin/kill")' 'Command::new("${pkgs.coreutils}/bin/kill")'
            '';
            cargoBuildFlags = [ "--bin" "kureha-code" ];
            cargoTestFlags = [ "--lib" "--bin" "kureha-code" ];
            FONTCONFIG_FILE = fontConfig;
          };
          platform = if pkgs.stdenv.isDarwin then "macos" else "linux";
          bundleName = if pkgs.stdenv.isDarwin then "${app.name}.app" else app.slug;
        in
        pkgs.runCommand "kureha-code-${manifest.package.version}" {
          nativeBuildInputs = [ packager ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.rcodesign ]
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.makeWrapper ];
          meta = {
            inherit (manifest.package) description;
            license = pkgs.lib.licenses.mit;
            platforms = systems;
            mainProgram = "kureha-code";
          };
        } (''
          appcoat package --manifest ${src}/appcoat.toml \
            --platform ${platform} --artifact ${binary}/bin/kureha-code \
            --output "$TMPDIR/"${pkgs.lib.escapeShellArg bundleName}
          mkdir -p "$out/bin"
        '' + pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
          rcodesign -C /dev/null sign "$TMPDIR/"${pkgs.lib.escapeShellArg bundleName}
          mkdir -p "$out/Applications"
          cp -R "$TMPDIR/"${pkgs.lib.escapeShellArg bundleName} "$out/Applications/"
          ln -s "../Applications/"${pkgs.lib.escapeShellArg "${app.name}.app/Contents/MacOS/${app.executable}"} "$out/bin/kureha-code"
        '' + pkgs.lib.optionalString pkgs.stdenv.isLinux ''
          cp -R "$TMPDIR/"${pkgs.lib.escapeShellArg bundleName}"/." "$out/"
          wrapProgram "$out/bin/kureha-code" \
            --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibraries pkgs)} \
            --set-default FONTCONFIG_FILE ${fontConfig}
        '');
      shell = system:
        let
          pkgs = import nixpkgs { inherit system; };
          rust = scarlet-rust-toolchain.packages.${system}.scarlet-rust-toolchain;
          runtime = runtimeLibraries pkgs;
        in
        pkgs.mkShell ({
          packages = [
            rust
            appcoat.packages.${system}.appcoat
            pkgs.cargo-about
            pkgs.rustfmt
            pkgs.git
            pkgs.python3
            pkgs.pkg-config
            pkgs.fontconfig
            pkgs.dejavu_fonts
          ]
          # Release packaging points Linux binaries at the system loader.
          ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.patchelf ]
          # Release packaging signs and notarizes the macOS disk image; openssl
          # converts the Developer ID .p12 for rcodesign.
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.rcodesign (pkgs.lib.getBin pkgs.openssl) ]
          ++ runtime;

          FONTCONFIG_FILE = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
          RUST_SRC_PATH = "${rust}/lib/rustlib/src/rust/library";
          CARGO_NET_GIT_FETCH_WITH_CLI = "true";
          CARGO_BUILD_JOBS = "4";
          CARGO_PROFILE_DEV_DEBUG = "0";
          CARGO_PROFILE_TEST_DEBUG = "0";
          SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          GIT_SSL_CAINFO = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

          shellHook = ''
            export PATH="${rust}/bin:${pkgs.rustfmt}/bin:$PATH"
            # Honor a host-provided CA bundle while keeping TLS verification enabled.
            export SSL_CERT_FILE="''${NIX_SSL_CERT_FILE:-$SSL_CERT_FILE}"
            export GIT_SSL_CAINFO="$SSL_CERT_FILE"
          '' + pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            if [ -z "''${XDG_RUNTIME_DIR:-}" ]; then
              export XDG_RUNTIME_DIR="/tmp/kureha-code-runtime-$(id -u)"
              mkdir -p "$XDG_RUNTIME_DIR"
              chmod 700 "$XDG_RUNTIME_DIR"
            fi
          '';
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtime;
        });
    in
    {
      packages = nixpkgs.lib.genAttrs systems (system:
        let kureha-code = package system;
        in { default = kureha-code; inherit kureha-code; });
      devShells = nixpkgs.lib.genAttrs systems (system: { default = shell system; });
    };
}
