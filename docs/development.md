# Development

stashee is Rust, GTK4, libadwaita and VTE (the terminal engine of GNOME Terminal and Ptyxis).
No Electron, no webviews, no daemons.

```sh
# Fedora
sudo dnf install gcc gcc-c++ rust cargo just tmux gtk4-devel libadwaita-devel vte291-gtk4-devel
git clone https://github.com/eeegoloauq/stashee-terminal && cd stashee-terminal
just install        # release build → ~/.local/bin/stashee, desktop entry and icons
```

Fedora Workstation already has GTK4, libadwaita and VTE at runtime; tmux must be installed.

`just check` runs the full gate (fmt, clippy, tests) that CI expects. How the app works:
[internals](internals.md). Issues and PRs are welcome.
[CONTRIBUTING.md](../CONTRIBUTING.md) lists the build dependencies and guidelines. For anything
bigger than a fix, open an issue first.
