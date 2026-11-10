{
  description = "Kureha Code development shell";

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
    scarlet-rust-toolchain.url = "github:petitstrawberry/scarlet-rust-nix/2b4ddd555389a8c53806bbc2586440b2662c38dd";
  };

  outputs = { nixpkgs, scarlet-rust-toolchain, appcoat, ... }:
    let
      # These are the host platforms provided by the pinned Scarlet toolchain.
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      shell = system:
        let
          pkgs = import nixpkgs { inherit system; };
          rust = scarlet-rust-toolchain.packages.${system}.scarlet-rust-toolchain;
          runtime = with pkgs; lib.optionals stdenv.isLinux [
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
          ] ++ runtime;

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
      devShells = nixpkgs.lib.genAttrs systems (system: { default = shell system; });
    };
}
