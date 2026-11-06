# Internals

How Stashee works, for contributors. Keep this in sync with the code in the
same change.

## Process model

One `adw::Application` (app id `dev.stashee.Terminal`), single instance via
D-Bus, GApplication's default. `stashee <workflow>` from a second process
arrives through `command-line`; the running instance opens or focuses the
workflow and the second process exits. `stashee list`, `config`, `help` and
`--version` never reach GApplication: they are answered locally, with no
display.

The app owns only tmux *clients*. Sessions belong to the tmux server on the
`stashee` socket (local panes) or on the remote host (SSH panes). That is what
makes stashing free: when the app exits, clients detach and sessions live on.

## Crates

The split is load-bearing. `stashee-core` has no UI dependency: it is the
kernel any future frontend would reuse. `stashee-pty` is the only crate allowed
`unsafe`; the workspace forbids `unsafe_code`, and every libc call lives there
behind a safe function.

```
crates/
  stashee-core/     no GTK, enforced by Cargo
    layout.rs       pane count → rows and cell rects (pure)
    model.rs        Workflow, PaneSpec, PaneKind
    tmux.rs         session naming, argv for create/attach/rename/kill/inspect
    ssh.rs          ssh/scp argv, remote tmux, exit-status meanings
    state.rs        state.toml load/save, startup reconciliation
    config.rs       config.toml, defaults, the commented template
    osc52.rs        OSC 52 copy extraction from a byte stream (pure)
  stashee-pty/      openpty, controlling tty, raw mode, SIGWINCH, malloc_trim
  stashee-voice/    dictation, no GTK
    lib.rs          pw-record capture, levels, the Backend trait
    parakeet.rs     Parakeet-TDT over ort; greedy decode ported from
                    transcribe-rs (MIT), mel features come from nemo128.onnx
    model.rs        pinned model manifest (size + sha256), consented download
    stt.rs          worker thread; unloads the model after idle minutes
  stashee/          the GTK4 app
    main.rs         CLI dispatch, Application setup, CSS, dark scheme
    cli.rs          displayless commands and the first-run greeting
    window.rs       one view (grid + attached panes) per workflow, actions,
                    sidebar and title sync, save
    window/panes.rs add, remove, close, swap, move, the SSH host dialog
    window/workflows.rs  create, rename, stash toggle, folder, delete
    window/sessions.rs   sessions follow names: all-or-nothing local
                         renames, then remote ones
    close_guard.rs  is the pane busy? (tmux, ssh, /proc)
    pane.rs         Pane widget: VTE terminal, spawn, SSH reconnect
    grid.rs         TilingGrid: animated tiling
    sidebar.rs      workflow list, Alt+N slots, context menu
    keys.rs         [keys] bindings → the window's key controller
    shortcuts.rs    the Keyboard Shortcuts dialog, built from live config
    settings.rs     runtime backdrop opacity, config.toml live reload
    proxy.rs        --osc52-proxy: pty relay wrapping every pane
    clipboard.rs    socket receiving the proxy's OSC 52 copies (0700 dir,
                    0600 socket, peer uid checked)
    dnd.rs          file drops and image pastes become typed paths (scp first
                    on SSH panes)
    voice.rs        record toggle, the waveform pill, model download consent
    paths.rs        XDG paths, the bundled tmux.conf
    data/           style.css (all glass tokens), tmux.conf, desktop entry,
                    icons, AppStream metainfo
```

## Data model

```rust
struct Workflow {
    name: String,          // unique by tmux slug ("My Work" == "my-work")
    default_dir: PathBuf,  // where new panes open
    stash: bool,           // default true; false = plain shells
    panes: Vec<PaneSpec>,  // display order
}

struct PaneSpec {
    id: String,                // short random suffix, stable across restarts
    kind: PaneKind,            // Local | Ssh { host, cwd }
    last_dir: Option<PathBuf>, // respawn directory
    run: Option<String>,       // template command, runs on session creation
}
```

The tmux session name is derived, never stored:
`stashee-{sanitize(workflow.name)}-{id}`.

## Startup reconciliation

State is a hint; tmux is the truth for local panes.

1. Load `state.toml` (a corrupt file is backed up as `state.toml.bak`).
2. List live sessions on the `stashee` socket.
3. Per workflow:
   - drop local specs whose session is gone, unless the machine rebooted since
     the last save (the kernel boot id changed): then keep them, and
     `new-session -A` respawns each as a fresh shell in its directory;
   - adopt live sessions with our prefix that state doesn't know (crashes,
     sessions made by hand);
   - keep SSH specs (liveness is unknown until connect);
   - workflows with stashing off respawn plain shells, except panes whose
     session is still alive: live sessions always reattach.
4. Save, build the UI, attach every pane.

## Panes

Each pane is one `vte4::Terminal` running one tmux client, wrapped in
`stashee --osc52-proxy` (VTE drops OSC 52 clipboard writes; the proxy relays
bytes untouched and sends copies to the app over a unix socket).

- Local: `tmux -L stashee -f {tmux.conf} start-server ; set -s set-clipboard
  on ; new-session -A -s {name} -c {dir}`.
- SSH: `ssh -t {host} -- tmux … new-session -A -s {name} \; set-option -t
  {name} mouse on`. Exit 127 (no remote tmux) falls back to plain `ssh` with a
  banner. Exit 255 or a signal is a lost connection: the pane stays and
  reattaches with backoff (1 s doubling to 16 s).
- Plain (stashing off): the user's `$SHELL`.

A pane's mode is never persisted: it is decided at build time (the workflow
stashes, or the pane's session is already alive) and carried on the runtime
`Pane`. Close, rename and delete consult the pane, so flipping the toggle never
orphans a live session.

Panes of inactive workflows stay attached, so `child-exited` has one meaning:
the session ended. The handler removes the pane from state and relayouts.

**Closing.** The close key first asks `close_guard` what the pane runs, off the
main thread: `tmux display-message -p '#{pane_current_command}'` for local and
SSH tmux panes, the shell's foreground process group in `/proc` for plain
panes. An idle shell closes at once; anything else, or a pane that cannot be
checked (mid-reconnect, a plain ssh connection), gets a confirmation. Closing
kills the tmux session and lets the resulting `child-exited` drive the UI.

## Tiling

`layout::layout(n)` returns panes per row (7 → `[3, 2, 2]`); table-driven tests
pin n = 1..=20. `TilingGrid` allocates children from those rects and animates
changes with an `adw::TimedAnimation` (180 ms ease-out): survivors slide, new
panes fade in. AdwAnimation follows the system animation setting.

## Styling and config

`data/style.css` holds every glass token. The app forces libadwaita's dark
scheme: the tokens and the terminal palette are dark-only. The system
high-contrast setting adds a `reduced-transparency` class and an opaque token
set, live.

`config.toml` is typed in `stashee-core` with defaults and a commented template;
a test keeps `docs/config.toml.example` equal to it. The app watches the file
and applies changes live (opacity, font, key bindings). Workflow templates are
read only when a workflow is created.

## Errors

`anyhow` through app plumbing; no `unwrap`/`expect` outside tests. User-facing
failures become a toast or an in-pane banner; everything else goes to
`tracing`, filtered by `STASHEE_LOG`.

## Dependencies

Every crate needs a reason.

| Crate | Where | Why |
|---|---|---|
| `gtk4`, `libadwaita`, `vte4` | app | toolkit, GNOME widgets and animation, terminal engine |
| `tracing-subscriber` | app | log output |
| `serde`, `toml` | core | config and state files |
| `fastrand` | core | pane ids |
| `anyhow`, `tracing` | all | errors and logs |
| `libc` | pty | pty syscalls, `malloc_trim` |
| `ort` | voice | ONNX Runtime; pinned exact (2.0 release candidate), behind the default `stt` feature |

Audio capture (`pw-record`) and the model download (`curl`) are subprocesses,
the same thin-client pattern as tmux and ssh.

## Testing

- `cargo test --workspace`: layout, argv assembly and naming, reconciliation,
  config, OSC 52 parsing, the close guard, voice helpers.
- `stashee-core/tests/tmux_roundtrip.rs` drives a real tmux on a private
  `TMUX_TMPDIR` (skipped without tmux).
- `stashee/tests/stash.rs` runs the real binary under `xvfb-run` and
  `dbus-run-session` in a private HOME and tmux dir: start, kill the app's
  process group, check the shell survived, start again, check it reattached.
  Skipped without those tools unless `STASHEE_E2E=require` (CI sets it).
  Signal process groups through `stashee_pty::terminate_group`, never a `kill`
  command: procps-ng 4.0.4 turns `kill -TERM -<pgid>` into a signal to every
  process.
- `stashee/tests/packaging.rs` keeps the COPR spec and metainfo versions equal
  to Cargo's; CI also runs `desktop-file-validate` and `appstreamcli validate`.
- Voice engine, by hand: `cargo run -p stashee-voice --example transcribe --
  <model-dir> <audio.raw>` (16 kHz mono s16le).
- Before a release, by hand: five panes across two workflows (one SSH), quit,
  `tmux -L stashee ls`, relaunch, everything reattaches in order; `exit` in a
  pane reflows the grid.

## Build and packaging

- The `stt` feature makes `ort` download onnxruntime at build time, so package
  builds need network (COPR: enable network) or `--no-default-features`.
  Speech models are never packaged.
- The binary is self-contained (`style.css` and `tmux.conf` are embedded).
  Installed files: binary, desktop entry, icons, AppStream metainfo, license.
  Metainfo screenshots point at `docs/shots/` on the last tag where the images
  changed; validate with `appstreamcli` after editing.
- Releasing: `just release <x.y.z> notes.md` bumps Cargo.toml, the COPR spec and
  the metainfo, runs the gate, commits and tags. Pushing the tag builds the
  GitHub release packages (republished to the Forgejo registry by
  `.forgejo/workflows/publish.yml`) and stamps and pushes the AUR PKGBUILD
  (`.github/workflows/aur.yml`). COPR builds from the tag tarball.
