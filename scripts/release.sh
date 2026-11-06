#!/usr/bin/env bash
# Release helper. The version lives in Cargo.toml and is duplicated in
# the COPR spec and the AppStream metainfo; this script is the only thing
# that touches those copies. The AUR PKGBUILD is stamped by
# .github/workflows/aur.yml when the tag reaches GitHub.
#
#   scripts/release.sh <x.y.z> [notes.md]
#       bump everything, run the gate, commit, tag. The annotated tag's
#       message is the release notes: Forgejo shows it on the releases
#       page, the GitHub workflow copies it into the release body.
#       Without a notes file, $EDITOR opens. One summary line, blank
#       line, then plain bullets.
set -euo pipefail
cd "$(dirname "$0")/.."

metainfo=crates/stashee/data/dev.stashee.Terminal.metainfo.xml
copr_spec=packaging/copr/stashee.spec

die() { echo "error: $*" >&2; exit 1; }

[ $# -ge 1 ] || die "usage: scripts/release.sh <x.y.z> [notes.md]"
ver=$1
notes=${2:-}
[[ "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "not a version: $ver"
[ -z "$notes" ] || [ -s "$notes" ] || die "notes file missing or empty: $notes"
[ -z "$(git status --porcelain)" ] || die "working tree not clean"
git rev-parse -q --verify "refs/tags/v$ver" >/dev/null && die "tag v$ver already exists"

sed -i "s/^version = \".*\"/version = \"$ver\"/" Cargo.toml
cargo update --workspace --quiet
sed -i "s/^Version:.*/Version:        $ver/" "$copr_spec"
sed -i "s|<releases>|<releases>\n    <release version=\"$ver\" date=\"$(date +%F)\"/>|" "$metainfo"

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
if command -v appstreamcli >/dev/null; then
  appstreamcli validate --no-net "$metainfo"
fi

git add Cargo.toml Cargo.lock "$copr_spec" "$metainfo"
git commit -m "stashee v$ver"
if [ -n "$notes" ]; then
  git tag -a "v$ver" -F "$notes"
else
  git tag -a "v$ver"
fi
echo "done — next: git push origin main v$ver"
