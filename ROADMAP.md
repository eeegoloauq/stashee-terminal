# Roadmap

Larger work that shouldn't be done in passing. Remove an item when it ships.

- **Simplify workflow operations.** `crates/stashee/src/window.rs` handles startup, views, actions and workflows in one large file; move and rename duplicate tmux rename and error handling.
- **Unify AUR releases.** `scripts/release.sh` has a manual checksum/update path that overlaps with `.github/workflows/aur.yml` tag publication.
- **Test critical integrations.** Add focused coverage for GTK interactions, live tmux/SSH behavior and packaging/release paths; current tests mainly cover helpers in `crates/stashee*`.
- **Review clipboard socket access.** `crates/stashee/src/clipboard.rs` accepts bounded socket payloads without a visible peer credential check; assess the local trust boundary.
- **Improve workflow discovery.** `crates/stashee/src/sidebar.rs` offers a scrollable fixed-width list without search or filtering; long names are ellipsized and directories appear only in tooltips.
