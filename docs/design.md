# Design

## Maintenance

Internal work that shouldn't be done in passing. Remove an item when it ships. How the
code works today: [internals](internals.md).

- **Test SSH panes end to end.** Reconnect decisions and backoff are unit-tested (`stashee-core/src/ssh.rs`) and the local stash round trip runs against the real app (`crates/stashee/tests/stash.rs`), but nothing drives a real sshd: reconnect after a dropped link, the no-tmux fallback, scp uploads.
