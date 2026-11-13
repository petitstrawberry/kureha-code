# direnv example extension

Gives the programs Kureha Code starts for the open folder the environment
[direnv](https://direnv.net) loads for it: language servers, new terminals,
and the programs other extensions run. With `use flake` in `.envrc`, that is
your Nix dev shell, so `rust-analyzer`, `nil` or `ruff` from the flake work
without starting the editor from `nix develop`. Written with the minimal SDK
in [`../sdk`](../sdk).

It is asked when a folder with an `.envrc` opens, and again when you save
`.envrc`, `flake.nix`, `flake.lock`, `shell.nix`, `default.nix` or
`devenv.nix` in that folder, or run "Environment: Reload". Language servers
restart with the new environment and wait while it loads (the first `use
flake` can build for minutes). Terminals that are already open keep their
environment; new ones get it.

Nothing happens until you allow two things from the command palette:

- "Extensions: Allow example.direnv to Run direnv", to run `direnv`;
- "Extensions: Allow example.direnv to Set the Environment", to give its
  variables to every program Kureha starts. Changing `PATH` decides which
  program runs for every name, so this is as strong as running code.

direnv's own approval still applies. A new or changed `.envrc` loads nothing
until you run "direnv: Allow .envrc" (the same as `direnv allow` in the folder).

`direnv` itself is found on the editor's `PATH`. When Kureha Code starts from
Finder, that `PATH` is short (`/usr/bin:/bin:/usr/sbin:/sbin`) and may not
include your Nix profile; start it from a terminal in that case.

```sh
nix develop --command cargo build --release --target wasm32-unknown-unknown \
  --manifest-path examples/extensions/direnv/Cargo.toml
mkdir -p ~/.config/kureha-code/extensions
ln -s "$PWD/examples/extensions/direnv" ~/.config/kureha-code/extensions/direnv
```
