# Task runner (https://just.systems). Plain cargo works too — see CLAUDE.md.

default: check

# fmt + clippy + tests, the full pre-change gate
check: fmt-check clippy test

build:
    cargo build --workspace

test:
    cargo test --workspace

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

# release build + user-level install: binary, .desktop, icon, `st` symlink
# (symlink skipped if an `st` already exists — suckless terminal collides)
install:
    cargo build --release --workspace
    install -Dm755 target/release/stashee ~/.local/bin/stashee
    [ -e ~/.local/bin/st ] || ln -s stashee ~/.local/bin/st
    install -Dm644 crates/stashee/data/dev.stashee.Terminal.desktop ~/.local/share/applications/dev.stashee.Terminal.desktop
    install -Dm644 crates/stashee/data/dev.stashee.Terminal.svg ~/.local/share/icons/hicolor/scalable/apps/dev.stashee.Terminal.svg
    -update-desktop-database ~/.local/share/applications

uninstall:
    rm -f ~/.local/bin/stashee
    [ "$(readlink ~/.local/bin/st 2>/dev/null)" = stashee ] && rm ~/.local/bin/st || true
    rm -f ~/.local/share/applications/dev.stashee.Terminal.desktop
    rm -f ~/.local/share/icons/hicolor/scalable/apps/dev.stashee.Terminal.svg
    -update-desktop-database ~/.local/share/applications
