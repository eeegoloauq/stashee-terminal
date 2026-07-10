#!/usr/bin/env bash
# Release helper. The version lives in Cargo.toml and is duplicated in
# the COPR spec, the AUR PKGBUILD/.SRCINFO and the AppStream metainfo;
# this script is the only thing that touches those copies.
#
#   scripts/release.sh <x.y.z> [notes.md]
#                                bump everything, run the gate, commit, tag.
#                                The annotated tag's message is the release
#                                notes: Forgejo shows it on the releases
#                                page, the GitHub workflow copies it into
#                                the release body. Without a notes file,
#                                $EDITOR opens. One summary line, blank
#                                line, then plain bullets.
#   scripts/release.sh aur       after the tag is on GitHub: update the AUR
#                                checksum + .SRCINFO and commit
set -euo pipefail
cd "$(dirname "$0")/.."

metainfo=crates/stashee/data/dev.stashee.Terminal.metainfo.xml
copr_spec=packaging/copr/stashee.spec
pkgbuild=packaging/aur/PKGBUILD
srcinfo=packaging/aur/.SRCINFO
tarball_url() { echo "https://github.com/eeegoloauq/stashee-terminal/archive/v$1.tar.gz"; }

die() { echo "error: $*" >&2; exit 1; }

current_version() {
  sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1
}

case "${1:-}" in
"")
  die "usage: scripts/release.sh <x.y.z> [notes.md] | aur"
  ;;

aur)
  ver=$(current_version)
  url=$(tarball_url "$ver")
  tmp=$(mktemp)
  trap 'rm -f "$tmp"' EXIT
  echo "fetching $url"
  curl -fsSL --retry 5 --retry-delay 10 --retry-all-errors -o "$tmp" "$url" \
    || die "tarball not on GitHub yet — push main and the tag first"
  sha=$(sha256sum "$tmp" | cut -d' ' -f1)
  sed -i "s/^pkgver=.*/pkgver=$ver/; s/^pkgrel=.*/pkgrel=1/; s/^sha256sums=.*/sha256sums=('$sha')/" "$pkgbuild"
  sed -i "s/^\tpkgver = .*/\tpkgver = $ver/; s/^\tpkgrel = .*/\tpkgrel = 1/; s|^\tsource = .*|\tsource = stashee-$ver.tar.gz::$url|; s/^\tsha256sums = .*/\tsha256sums = $sha/" "$srcinfo"
  git add "$pkgbuild" "$srcinfo"
  git diff --cached --quiet && { echo "AUR files already at $ver — nothing to do"; exit 0; }
  git commit -m "aur: $ver"
  echo "done — push main, then copy PKGBUILD + .SRCINFO into the AUR repo and push it"
  ;;

*)
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
  echo "done — next: git push origin main v$ver, then scripts/release.sh aur"
  ;;
esac
