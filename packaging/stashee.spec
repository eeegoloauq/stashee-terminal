# Binary-only spec: CI builds the release binary with cargo, this spec
# just stages files. Invoke with:
#   rpmbuild -bb packaging/stashee.spec \
#     --define "version <x.y.z>" --define "srcroot <checkout dir>"
%define debug_package %{nil}

Name:           stashee
Version:        %{version}
Release:        1%{?dist}
Summary:        Glass-styled tiling terminal workspace over tmux
License:        MIT
URL:            https://github.com/eeegoloauq/stashee-terminal

Requires:       tmux
Recommends:     wl-clipboard
Recommends:     xclip

%description
Glass-styled tiling terminal workspace for Linux. Terminals are grouped
into named workflows and tile automatically. Every terminal runs inside
a tmux session, so closing the app stashes a workflow instead of
killing it — reopen, and every shell is back exactly where it was.

%install
install -Dm755 %{srcroot}/target/release/stashee %{buildroot}%{_bindir}/stashee
install -Dm644 %{srcroot}/crates/stashee/data/dev.stashee.Terminal.desktop %{buildroot}%{_datadir}/applications/dev.stashee.Terminal.desktop
install -Dm644 %{srcroot}/crates/stashee/data/dev.stashee.Terminal.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/dev.stashee.Terminal.svg
install -Dm644 %{srcroot}/crates/stashee/data/dev.stashee.Terminal.metainfo.xml %{buildroot}%{_metainfodir}/dev.stashee.Terminal.metainfo.xml
install -Dm644 %{srcroot}/LICENSE %{buildroot}%{_datadir}/licenses/%{name}/LICENSE

%files
%license %{_datadir}/licenses/%{name}/LICENSE
%{_bindir}/stashee
%{_datadir}/applications/dev.stashee.Terminal.desktop
%{_datadir}/icons/hicolor/scalable/apps/dev.stashee.Terminal.svg
%{_metainfodir}/dev.stashee.Terminal.metainfo.xml
