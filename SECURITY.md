# Security

Found a vulnerability? Please report it privately through
[GitHub security advisories](https://github.com/eeegoloauq/stashee-terminal/security/advisories/new)
instead of opening a public issue. You'll get a response within a few days, and a fix lands in
the next release — only the latest release is supported.

Scope worth knowing: stashee is a local desktop app — it opens no network ports and runs no
daemons of its own. Shells run inside your own tmux server, and SSH panes use the system `ssh`
client and your existing configuration. Dependencies are pinned via `Cargo.lock`.
