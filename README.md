<div align="center">

# stashee

**A glass tiling terminal workspace for Linux — shells never die by accident.**

<img src="docs/shots/welcome.png" alt="stashee on first launch" width="85%">

<br>

Terminals live in named **workflows** and tile themselves.
Every pane runs inside tmux, so closing the app **stashes** a workflow
instead of killing it — reopen, and every shell is back exactly where
it was.

<br>

<br>

<img src="docs/shots/agents.png" alt="two Claude Code agents in a stashed workflow" width="85%">

*Two coding agents grinding away in a `dev` workflow. Close the
window — they don't even notice.*

<br>

</div>

## Why

- **Nothing to lose.** Quit, crash, update — sessions live in tmux,
  not in the app. The window is just a beautiful client.
- **Zero layout management.** `Ctrl+T`, and the grid tiles itself —
  up to three columns, then rows, always evenly split, animated.
- **SSH panes are stashed too.** A pane on a remote host survives
  reboots and dropped connections; even remote copy lands in your
  local clipboard.
- **Native and light.** Rust + GTK4 + libadwaita + VTE — the same
  terminal engine as GNOME Terminal and Ptyxis. No Electron, no
  webviews, no daemons of our own.

## Feel

| | |
|---|---|
| `stashee work` | jump to the "work" workflow from any shell |
| `Ctrl+T` | new pane — it finds its place |
| `Ctrl+Shift+T` | new SSH pane |
| `Alt+1…9` | switch workflow |
| `Ctrl+W` | the only way a pane dies on purpose |
| `stashee config` | every setting, one file, applied live |

There is no settings GUI, no plugins, no themes gallery. The app does
very little, very well — that's the product.

## Status

**Pre-alpha.** v1 targets Fedora + GNOME/Wayland; other distros and
compositors after that. The core is UI-agnostic by design
(`stashee-core` has no GTK in it) — other platforms arrive later as
thin native frontends, never a webview.

## Install

Every release ships native packages on the
[releases page](https://github.com/eeegoloauq/stashee-terminal/releases):
an `.rpm` for Fedora and a `.pkg.tar.zst` for Arch.

```sh
# Fedora
sudo dnf install ./stashee-*.rpm

# Arch
sudo pacman -U ./stashee-*.pkg.tar.zst
```

tmux does the stashing: install it with your package manager if it is
not there already.

## Build

```sh
# Fedora
sudo dnf install gcc rust cargo gtk4-devel libadwaita-devel vte291-gtk4-devel
git clone https://github.com/eeegoloauq/stashee-terminal && cd stashee-terminal
just install        # release build → ~/.local/bin/stashee (+ st symlink)
```

Runtime needs nothing extra on Fedora Workstation — GTK4, libadwaita
and VTE already ship with it.

## License

[MIT](LICENSE).
