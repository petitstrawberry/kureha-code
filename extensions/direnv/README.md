# direnv extension

Gives the programs Kureha Code starts for the open folder the environment
[direnv](https://direnv.net) loads for it: language servers, new terminals,
and the programs other extensions run. With `use flake` in `.envrc`, that is
your Nix dev shell, so `rust-analyzer`, `nil` or `ruff` from the flake work
without starting the editor from `nix develop`. Written with the minimal SDK
in [`examples/extensions/sdk`](../../examples/extensions/sdk).

It is asked when a folder with an `.envrc` opens, and again when you save
`.envrc`, `flake.nix`, `flake.lock`, `shell.nix`, `default.nix` or
`devenv.nix` in that folder, or run "Environment: Reload". Language servers
restart with the new environment and wait while it loads (the first `use
flake` can build for minutes). Terminals that are already open keep their
environment; new ones get it.

Nothing happens until you allow two things from the command palette:

- "Extensions: Allow kureha.direnv to Run direnv", to run `direnv`;
- "Extensions: Allow kureha.direnv to Set the Environment", to give its
  variables to every program Kureha starts. Changing `PATH` decides which
  program runs for every name, so this is as strong as running code.

direnv's own approval still applies. A new or changed `.envrc` loads nothing
until you run "direnv: Allow .envrc" (the same as `direnv allow` in the folder).

`direnv` itself is found on the editor's `PATH`. Started from Finder or the
Dock, Kureha Code reads your login shell's environment first (as a terminal
would have it), so a `direnv` from your Nix profile or Homebrew is found.

To install it, run this from the repository and restart Kureha Code:

```sh
nix develop --command python3 scripts/build-extension.py --install extensions/direnv
```

It builds the module and puts `extension.toml`, `extension.wasm` and this
README in `~/.config/kureha-code/extensions/direnv.kx/` (or under
`KUREHA_EXTENSIONS_DIR`). Run it again after changing the code; `--out <folder>`
puts the folder somewhere else.
