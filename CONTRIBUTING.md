# Contributing to stashee

First off — thank you for considering a contribution! Bug reports from
different distros and desktops are especially valuable: terminal + GTK +
tmux is a combination with a lot of environment-specific corners.

## One thing to know before proposing features

The scope of stashee is **deliberately small**: no settings GUI, no plugin
system, no theme gallery, no layout management. That's a feature, and it's
defended. So for anything bigger than a fix, **open an issue first** and
let's talk it over — it may save you an evening of work on a PR that gets
a friendly "no". Fixes and portability patches need no prior discussion.

## Reporting bugs

Open a [bug report](https://github.com/eeegoloauq/stashee-terminal/issues/new/choose).
The environment details in the form (distro, Wayland/X11, how you installed,
tmux version) aren't bureaucracy — most stashee bugs only reproduce in a
specific combination of them.

Security issues don't belong in the issue tracker — see [SECURITY.md](SECURITY.md).

## Development setup

You need Rust 1.97 or newer ([`rust-toolchain.toml`](rust-toolchain.toml) selects stable), tmux, and
the GTK stack:

```sh
# Fedora
sudo dnf install gcc gcc-c++ rust cargo just gtk4-devel libadwaita-devel vte291-gtk4-devel tmux
# Arch
sudo pacman -S --needed rust just gtk4 libadwaita vte4 tmux

git clone https://github.com/eeegoloauq/stashee-terminal && cd stashee-terminal
cargo build --workspace
```

`just install` builds a release binary and installs it for your user
(`~/.local/bin/stashee`, plus the desktop entry and icons); `just uninstall`
removes it.

Stashee is Rust, GTK4, libadwaita and VTE (the terminal engine of GNOME
Terminal and Ptyxis), with no webview. The workspace has four crates:

- [`stashee`](crates/stashee): the GTK app.
- [`stashee-core`](crates/stashee-core): workflows, tiling math, the tmux/ssh command model and saved
  state. No UI toolkit; most tests live here.
- [`stashee-pty`](crates/stashee-pty): the only crate allowed `unsafe`: pty plumbing for the OSC 52
  clipboard proxy.
- [`stashee-voice`](crates/stashee-voice): experimental dictation (PipeWire capture, local Parakeet
  speech recognition). `--no-default-features` builds without the speech
  engine.

## Before you push

```sh
just check    # fmt --check, clippy -D warnings, tests — the exact CI gate
```

Plain cargo works too (`cargo fmt --all`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`) if you don't use
[just](https://just.systems). Clippy warnings are errors in CI, so a green
`just check` locally means CI will agree.

## Pull requests

- Keep changes small and focused — one topic per PR.
- `just check` must pass.
- Features: link the issue where we discussed it (see the scope note above).
- Say how you tested it — which distro, Wayland or X11.

## What to expect

This project has a single maintainer working on it in spare time. Issues and
PRs get read, but a response can take a few days — that's normal, not a brush-off.

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
