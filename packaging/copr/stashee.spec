# Source spec for COPR: builds from the GitHub tag tarball on Fedora's
# infrastructure. CI uses packaging/stashee.spec instead (binary-only).
# The COPR project must have network access enabled so cargo can fetch
# the locked crates. Bump Version together with the workspace Cargo.toml.
%define debug_package %{nil}

Name:           stashee
Version:        0.1.4
Release:        1%{?dist}
Summary:        Glass-styled tiling terminal workspace over tmux
License:        MIT
URL:            https://github.com/eeegoloauq/stashee-terminal
Source0:        %{url}/archive/v%{version}/stashee-terminal-%{version}.tar.gz

BuildRequires:  gcc
BuildRequires:  rust
BuildRequires:  cargo
BuildRequires:  gtk4-devel
BuildRequires:  libadwaita-devel
BuildRequires:  vte291-gtk4-devel
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

Requires:       tmux
Recommends:     wl-clipboard
Recommends:     xclip

%description
Terminals are grouped into named workflows and tile automatically.
Every pane runs inside a tmux session, so closing the app keeps every
shell running; reopening restores them exactly as they were.

%prep
%autosetup -n stashee-terminal-%{version}

%build
cargo build --release --locked --workspace

%install
install -Dm755 target/release/stashee %{buildroot}%{_bindir}/stashee
install -Dm644 crates/stashee/data/dev.stashee.Terminal.desktop %{buildroot}%{_datadir}/applications/dev.stashee.Terminal.desktop
install -Dm644 crates/stashee/data/dev.stashee.Terminal.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/dev.stashee.Terminal.svg
for s in 64 128 256; do
  install -Dm644 crates/stashee/data/dev.stashee.Terminal-$s.png %{buildroot}%{_datadir}/icons/hicolor/${s}x${s}/apps/dev.stashee.Terminal.png
done
install -Dm644 crates/stashee/data/dev.stashee.Terminal.metainfo.xml %{buildroot}%{_metainfodir}/dev.stashee.Terminal.metainfo.xml

%check
cargo test --locked --workspace
desktop-file-validate %{buildroot}%{_datadir}/applications/dev.stashee.Terminal.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/dev.stashee.Terminal.metainfo.xml

%files
%license LICENSE
%{_bindir}/stashee
%{_datadir}/applications/dev.stashee.Terminal.desktop
%{_datadir}/icons/hicolor/scalable/apps/dev.stashee.Terminal.svg
%{_datadir}/icons/hicolor/*/apps/dev.stashee.Terminal.png
%{_metainfodir}/dev.stashee.Terminal.metainfo.xml
