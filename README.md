# stashee

A tiling terminal workspace for Linux. Terminals are grouped into named workflows and tile
automatically. Every pane runs inside a tmux session, so closing the app stashes a workflow
instead of killing it, and reopening brings the shells back.

[![Release](https://img.shields.io/github/v/release/eeegoloauq/stashee-terminal?label=release)](https://github.com/eeegoloauq/stashee-terminal/releases/latest)
[![CI](https://github.com/eeegoloauq/stashee-terminal/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/eeegoloauq/stashee-terminal/actions/workflows/ci.yml)
[![Copr build status](https://copr.fedorainfracloud.org/coprs/eeegoloauq/stashee/package/stashee/status_image/last_build.png)](https://copr.fedorainfracloud.org/coprs/eeegoloauq/stashee/package/stashee/)
[![AUR version](https://img.shields.io/aur/version/stashee?label=AUR)](https://aur.archlinux.org/packages/stashee)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

![stashee on first launch](docs/shots/first-run.png)

## Features

- Sessions live in tmux, so quitting, crashing or updating the app loses nothing. Stashing can be
  turned off per workflow for plain shells.
- No layout management. New panes tile automatically: up to three
  columns, then rows, always evenly split.
- SSH panes are stashed too when the remote host has tmux: they survive local reboots and dropped
  connections, and copying on the remote host fills the local clipboard. Without remote tmux the
  pane is a plain SSH session.
- Drop a file or paste a screenshot into an SSH pane: it is copied to the remote host with scp and
  the remote path is typed at the prompt.
- Rust, GTK4, libadwaita and VTE (the terminal engine of GNOME Terminal and Ptyxis). No Electron,
  no webviews, no daemons.

![Two coding agents in a stashed workflow](docs/shots/agents.png)

A coding agent per project in one stashed workflow. Closing the window leaves both running.

## Usage

| | |
|---|---|
| `stashee work` | open the "work" workflow from any shell |
| `Ctrl+T` | new pane |
| `Ctrl+Shift+T` | new SSH pane |
| `Alt+1…9` | switch workflow |
| `Ctrl+W` | close pane (the only way to end one) |
| `Ctrl+Shift+V` | paste; a clipboard image becomes a file path, uploaded first on SSH panes |
| `stashee config` | open the config file; changes apply live ([all options](docs/config.toml.example)) |

There is no settings GUI and no plugin system.

## Voice input (experimental)

Local dictation, off by default: set `[voice] enabled = true` in the
config, press `Ctrl+Shift+Space`, speak, press it again. The
transcript is typed into the focused pane and is not sent automatically. Recognition runs locally
on the CPU with NVIDIA's Parakeet model (25 languages). The model is a one-time download of about
670 MB, after a confirmation dialog.

<img src="docs/shots/voice.png" alt="Dictating into a pane: the recording pill and the transcribed text" width="60%">

## Install

Fedora, via [COPR](https://copr.fedorainfracloud.org/coprs/eeegoloauq/stashee/):

```sh
sudo dnf copr enable eeegoloauq/stashee
sudo dnf install stashee
```

Arch, via the [AUR](https://aur.archlinux.org/packages/stashee):

```sh
yay -S stashee   # or: paru -S stashee
```

Each release on the
[releases page](https://github.com/eeegoloauq/stashee-terminal/releases)
also ships an `.rpm` for Fedora and a `.pkg.tar.zst` for Arch:

```sh
# Fedora
sudo dnf install ./stashee-*.rpm

# Arch
sudo pacman -U ./stashee-*.pkg.tar.zst
```

tmux is required locally, and on remote hosts for stashed SSH panes.

## Build

```sh
# Fedora
sudo dnf install gcc rust cargo just gtk4-devel libadwaita-devel vte291-gtk4-devel
git clone https://github.com/eeegoloauq/stashee-terminal && cd stashee-terminal
just install        # release build → ~/.local/bin/stashee, plus an `st` symlink if that name is free
```

Fedora Workstation already has GTK4, libadwaita and VTE at runtime; tmux must be installed.

## Contributing

Issues and PRs are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) lists the build dependencies and
guidelines. `just check` runs the full gate (fmt, clippy, tests) that CI expects. For anything
bigger than a fix, open an issue first.

## License

[MIT](LICENSE).
