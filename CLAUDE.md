# CLAUDE.md

stashee-terminal is a glass-styled tiling terminal workspace for Linux
(Fedora first). Terminals are grouped into named *workflows* and tile
automatically. Every terminal runs inside a tmux session, so closing the
app *stashes* a workflow instead of killing it — reopen, and every shell
is back exactly where it was.

Product behavior lives in [docs/SPEC.md](docs/SPEC.md). Code structure
lives in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Read the relevant
one before changing anything it covers; update it in the same change when
behavior or structure moves.

## Principles

**Think before coding.** State assumptions. If a task has two readings,
name them and pick one with a reason. If a simpler approach exists than
the one requested, say so before building the requested one. Push back
when something is wrong — agreement is not a service.

**Simplicity is the product.** The entire value of this app is doing very
little, very well. Every feature must survive the question: *would the app
still feel complete without this?* Prefer the boring, obvious
implementation. If a senior engineer would call it overcomplicated, it is.

**Surgical changes.** Touch what the task needs and nothing else. No
drive-by refactors, no opportunistic renames, no "while I was here". A
diff should read as one author with one intention.

**Quality over prototypes.** No quick hacks, no TODO-later scaffolding.
Whatever lands is finished: tested where testable, handling its errors,
styled like the code around it.

## Locked decisions

Settled. Do not relitigate unless the owner explicitly reopens them.

- **Stack: Rust everywhere; GTK4 + libadwaita + VTE (`vte4` crate) as
  the Linux frontend.** No Electron, no Tauri, no webviews — ever. We do
  not write terminal emulation: we embed the platform's native engine
  (VTE is what GNOME Terminal and Ptyxis run on). Cross-platform later
  means new native frontends over the same core crate — see
  "Cross-platform strategy" in docs/ARCHITECTURE.md.
- **Persistence belongs to tmux, not us.** Every pane — local or SSH — is
  a tmux session on a dedicated socket. The app is a thin, beautiful
  client. Any session state we could own, tmux owns instead. Stashing is
  per-workflow and **on by default**; a workflow can opt out
  (`stash = false`), making its panes plain shells that die with the
  app. There is no third mode.
- **Naming:** the binary is `stashee`; `st` is an optional install-time
  symlink (`st` itself collides with suckless terminal).
- **Voice-to-text is v2** (local whisper.cpp, push-to-talk), not v1.
- **v1 targets Fedora + GNOME/Wayland.** Other distros and compositors
  come later.

## Invariants

- Killing the app must never kill a shell in a stashed workflow (the
  default). The app owns *clients* (`tmux attach`); tmux owns sessions.
- `stashee-core` never depends on GTK or any UI toolkit — it is the
  future cross-platform kernel (workflows, layout, tmux, state).
- The source of truth for local panes is `tmux -L stashee ls`, reconciled
  with the state file at startup (state adds only ordering and SSH
  targets).
- Layout math is a pure function in `crates/stashee-core/src/layout.rs`
  — no GTK types, unit-tested.
- One process instance. A second `stashee <workflow>` invocation forwards
  its arguments to the running instance over D-Bus (GApplication default).

## Commands

System dependencies (once):

```
# Fedora
sudo dnf install gcc rust cargo gtk4-devel libadwaita-devel vte291-gtk4-devel
# Ubuntu/Debian (the headless dev server; rust via rustup)
sudo apt install pkg-config libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev
```

These are build-time headers for the dev machine only. End users
install nothing extra: the runtime libraries (GTK4, libadwaita, VTE)
already ship with Fedora Workstation — VTE is the engine of the system
terminal.

Day to day:

```
cargo build                  # debug build
cargo run                    # run the app
cargo test                   # unit tests (layout, tmux, state, config)
cargo clippy -- -D warnings  # lint, warnings are errors
cargo fmt                    # format
just install                 # release build + user-level install
```

Build, test, clippy, and fmt must all pass before a change is done.

## Conventions

- rustfmt defaults; clippy clean at `-D warnings`.
- No `unwrap()`/`expect()` outside tests. Errors surface as an in-app
  toast or land in the log (`tracing`) — never silently dropped.
- A new dependency needs a row with a one-line justification in the
  dependency table in docs/ARCHITECTURE.md. When in doubt, don't add it.
- Comments only for constraints the code cannot express; match the sparse
  style around you.
- Everything in the repo is English — code, docs, commit messages.
  Conversation with the owner is usually Russian; the repo isn't.

## What not to build (v1)

Tabs inside panes · plugin system · settings GUI · theme gallery · pane
drag-reordering · non-Linux support · our own scrollback (tmux's is fine
for v1; native scrollback via tmux control mode is a v1.x item — see the
roadmap in docs/SPEC.md).
