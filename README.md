# stashee

A tiling terminal for Linux that keeps your shells running after you close it.

[![Release](https://img.shields.io/github/v/release/eeegoloauq/stashee-terminal?label=release)](https://github.com/eeegoloauq/stashee-terminal/releases/latest)
[![CI](https://github.com/eeegoloauq/stashee-terminal/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/eeegoloauq/stashee-terminal/actions/workflows/ci.yml)
[![AUR version](https://img.shields.io/aur/version/stashee?label=AUR)](https://aur.archlinux.org/packages/stashee)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

![stashee on first launch](docs/shots/first-run.png)

- Named workflows survive quitting, crashes and updates
- Panes tile automatically, no layout to manage
- SSH panes survive disconnects when the host has tmux
- Drop a file into an SSH pane to upload it
- Local voice dictation (experimental)

## Install

Fedora, via [COPR](https://copr.fedorainfracloud.org/coprs/eeegoloauq/stashee/):

```sh
sudo dnf copr enable eeegoloauq/stashee && sudo dnf install stashee
```

Arch, via the [AUR](https://aur.archlinux.org/packages/stashee): `yay -S stashee`

Needs tmux. Package files are on the [latest release](https://github.com/eeegoloauq/stashee-terminal/releases/latest).

## Usage

`stashee work` opens the "work" workflow; `stashee config` opens the config file.

[Keys and features](docs/usage.md) · [Config options](docs/config.toml.example)

## Development

[Build and test](docs/development.md) · [Contributing](CONTRIBUTING.md) · [Roadmap](ROADMAP.md)

## License

[MIT](LICENSE)
