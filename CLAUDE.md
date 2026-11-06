# stashee-terminal

Stashee is a native Linux terminal for people who keep related shells in named,
tiled workflows. tmux owns persistent sessions: closing the app leaves stashed
shells running, and reopening restores them. How it works:
[docs/internals.md](docs/internals.md). Product behavior: `docs/SPEC.md` where
that local doc is available.

## Decisions and gotchas

- Use Rust, GTK4, libadwaita and VTE; `stashee-core` stays independent of UI
  toolkits. Do not add Electron, Tauri, webviews or terminal emulation.
- Stashing is enabled per workflow by default; `stash = false` uses plain
  shells. Live tmux sessions are the source of truth for local stashed panes;
  saved state supplies ordering and SSH targets.
- Fedora and GNOME/Wayland are the primary desktop target. Voice input is
  already present as an experimental, opt-in feature.
- `stashee` is the binary. tmux is required at runtime.
- `docs/SPEC.md` and `docs/ARCHITECTURE.md` are local symlinks into private
  notes, absent from other clones. `docs/internals.md` is the tracked
  architecture doc; update it when behavior or structure changes.

## Commands

```sh
cargo run -p stashee
cargo build --workspace
just check                      # fmt check, Clippy, tests
just install                    # release build and user-level install
```

All four must pass before a change is done. No `unwrap()`/`expect()` outside
tests; errors surface as a toast or a `tracing` log, never dropped. Feature bar:
if the app would feel complete without it, don't build it.

Equivalent checks: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace`.

## Release and rollback

`just release VERSION NOTES.md` runs checks, updates versions, commits and
creates an annotated `v*` tag; pushing the commit and tag is a separate step.
The tag triggers GitHub package/release builds and AUR publication, followed
by Forgejo republication. There is no automated rollback or post-release
health check. For a bad release, stop distributing its artifacts, restore the
last known good package for users, then publish a corrected version and verify
the installed app starts and reconnects to stashed shells. Do not reuse a
published tag.

Planned larger work: `ROADMAP.md`.
