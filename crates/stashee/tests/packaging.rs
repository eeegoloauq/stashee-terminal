//! The version is duplicated outside Cargo.toml (scripts/release.sh
//! keeps the copies in step); a release built from mismatched files
//! ships the wrong version to COPR or GNOME Software.

#![allow(clippy::unwrap_used)]

use std::path::Path;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn repo_file(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read_to_string(root.join(path)).unwrap()
}

#[test]
fn copr_spec_version_matches_cargo() {
    let spec = repo_file("packaging/copr/stashee.spec");
    let version = spec
        .lines()
        .find_map(|line| line.strip_prefix("Version:"))
        .map(str::trim);
    assert_eq!(version, Some(VERSION));
}

#[test]
fn newest_metainfo_release_matches_cargo() {
    let metainfo = repo_file("crates/stashee/data/dev.stashee.Terminal.metainfo.xml");
    let newest = metainfo
        .split("<release version=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next());
    assert_eq!(newest, Some(VERSION));
}
