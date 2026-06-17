# Cargo home lives in the workspace

Vixen points `CARGO_HOME` at `<workspace>/.cargo` instead of the default
`~/.cargo`. Everything Cargo would normally write to the user's home — the
registry index, downloaded crate sources, git checkouts, `cargo-binstall`-ed
tooling — stays inside the workspace tree.

## Why

- **Workspace is the unit of trust.** The registry cache, git checkouts,
  and installed binaries are all inputs to the build; keeping them under
  `<workspace>/.cargo` makes that explicit and lets the reviewer audit them
  alongside the source.
- **Reproducibility.** A fresh contributor gets the same view of the dep
  tree as CI; nothing depends on whatever happens to live in their
  `~/.cargo` from other projects.
- **No cross-project leakage.** Vixen's `cargo-binstall` packages don't
  shadow globally-installed copies on the user's machine, and vice versa.

## How it's wired

- `.mise.toml` `[env]` exports:
  - `CARGO_HOME = "{{ config_root }}/.cargo"`
  - `PATH = "{{ config_root }}/.cargo/bin:$PATH"` (so `cargo-audit`,
    `cargo-deny`, `cargo-fuzz` installed by `mise bootstrap` are runnable
    from `just audit` / `just fuzz-init`)
- `.gitignore` ignores everything under `.cargo/` **except** `config.toml`,
  which is the project-pinned Cargo config and ships with the repo.
- `.cargo/config.toml` is checked in. It doubles as the CARGO_HOME config
  (Cargo reads the same physical file in both roles) — keep it limited to
  project-pinned settings, never cache state.

`mise` exports these vars to any mise-active shell. After `mise trust`
the shell that ran `mise` (or was started via `mise shell`) sees them.
`mise exec -- <cmd>` works for one-shot invocations.

## Verifying it took effect

```sh
mise trust
echo "$CARGO_HOME"        # → /path/to/vixen/.cargo
ls .cargo/                # → config.toml, plus registry/, bin/, ... after first build
```

`cargo` itself reports the resolved home:

```sh
cargo config get          # honors CARGO_HOME from the env
```

## Disk

The cache for Vixen's dep tree (Stylo + mozjs + reqwest + …) is several
hundred MiB. It's all under `.cargo/` and git-ignored, so it costs nothing
in the repo; treat it like `target/`. `rm -rf .cargo` is safe and Cargo
will repopulate on the next build.

## Updating tooling installed via `cargo-binstall`

`mise bootstrap` runs `cargo-binstall` for `cargo-audit`, `cargo-deny`, and
`cargo-fuzz`. Because `CARGO_HOME` is workspace-local, those binaries land
in `.cargo/bin/`. To refresh:

```sh
mise exec -- cargo binstall --no-confirm --force cargo-audit cargo-deny cargo-fuzz
```

or rerun `mise bootstrap --yes`.

## Caveats

- **Editor integration.** rust-analyzer and IDEs spawn `cargo` themselves;
  make sure they inherit the mise env (`direnv` integration, or launch the
  editor from a mise-active shell). If they don't, they'll fall back to
  `~/.cargo` and re-download the registry there. Harmless, just slow.
- **Other projects on the host.** They keep using `~/.cargo` as before;
  Vixen's CARGO_HOME only applies inside a mise-active shell in this
  workspace.
