# stashee-terminal

Glass-styled tiling terminal workspace for Linux. Terminals group into named
*workflows*; every pane is a tmux session on a dedicated socket, so closing the
app *stashes* a workflow — reopen, and every shell is back.

Product behavior: docs/SPEC.md. Code structure: docs/ARCHITECTURE.md.
Read the relevant one before changing what it covers; update it in the same change.

## Locked decisions — don't relitigate

- Rust + GTK4 + libadwaita + VTE (`vte4`). No Electron/Tauri/webviews, no own
  terminal emulation. Cross-platform later = new native frontends over the same core crate.
- tmux owns persistence; the app is a thin client. Stashing is per-workflow,
  on by default; `stash = false` opts out to plain shells. No third mode.
- Binary `stashee`, `st` = optional install-time symlink. Voice-to-text is v2.
  v1 targets Fedora + GNOME/Wayland only.

## Invariants

- Killing the app never kills a shell in a stashed workflow (app owns clients, tmux owns sessions).
- `stashee-core` never depends on GTK or any UI toolkit.
- Source of truth for local panes is `tmux -L stashee ls`, reconciled with the
  state file at startup (state adds only ordering and SSH targets).
- Layout math is a pure function in `crates/stashee-core/src/layout.rs` — no GTK types, unit-tested.
- One process instance; a second invocation forwards its args over D-Bus (GApplication default).

## Commands

```
cargo build / run / test
cargo clippy -- -D warnings
cargo fmt
just install                 # release build + user-level install
```

All four (build, test, clippy, fmt) must pass before a change is done.
Build headers, Debian/Ubuntu dev box: `apt install pkg-config libgtk-4-dev
libadwaita-1-dev libvte-2.91-gtk4-dev` (end users need nothing — GTK/VTE ship with Fedora).

## Conventions

- No `unwrap()`/`expect()` outside tests. Errors surface as a toast or `tracing` log, never dropped.
- New dependency = row with one-line justification in the dep table in docs/ARCHITECTURE.md.
- Everything in the repo is English — code, docs, commits (owner chat is usually Russian).
- Feature bar: would the app still feel complete without it? If yes, don't build it.

## Not in v1

Tabs inside panes · plugins · settings GUI · theme gallery ·
non-Linux · own scrollback (tmux's is fine; control-mode scrollback is v1.x — roadmap in docs/SPEC.md).
