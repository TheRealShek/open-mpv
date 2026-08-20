%global debug_package %{nil}

Name:           open-mpv
Version:        0.1.0
Release:        1%{?dist}
Summary:        Minimalist local photo and video viewer
License:        MIT AND Apache-2.0 AND Apache-2.0 WITH LLVM-exception AND ISC AND MPL-2.0 AND Unicode-3.0
URL:            https://github.com/TheRealShek/open-mpv
Source0:        %{name}-%{version}.tar.gz
ExclusiveArch:  x86_64

BuildRequires:  ImageMagick
BuildRequires:  ShellCheck
BuildRequires:  appstream
BuildRequires:  cargo
BuildRequires:  dbus-daemon
BuildRequires:  desktop-file-utils
BuildRequires:  gcc
BuildRequires:  glycin-devel >= 2.1
BuildRequires:  gstreamer1-devel
BuildRequires:  gtk4-devel >= 4.22
BuildRequires:  rust
Requires:       glycin-loaders >= 2.1
Requires:       gstreamer1-plugin-gtk4
Requires:       gstreamer1-plugins-base
Requires:       gtk4 >= 4.22
Recommends:     gstreamer1-plugin-libav
# Supplies the Intel QSV decoders preferred by the target playback path.
Recommends:     gstreamer1-plugins-bad-free
# Supplies scaletempo for pitch-preserving playback speed.
Recommends:     gstreamer1-plugins-good

%description
open-mpv is a fast, distraction-free viewer for local photos and videos on
Fedora Workstation with GNOME and Wayland.

%prep
%autosetup

%build
RUSTFLAGS="${RUSTFLAGS} -Cstrip=symbols" cargo build --release --frozen --offline

%install
./install.sh --no-build --prefix %{_prefix} --destdir %{buildroot}
set +x
for crate_dir in vendor/*; do
    crate_name=$(basename "$crate_dir")
    for license_file in "$crate_dir"/LICENSE* "$crate_dir"/COPYING* \
        "$crate_dir"/NOTICE* "$crate_dir"/UNLICENSE*; do
        [ -f "$license_file" ] || continue
        # These crates offer MPL-2.0 or LGPL-2.1-or-later. This package uses
        # their MPL-2.0 option, matching the License tag above.
        case $(basename "$license_file") in
            *LGPL*) continue ;;
        esac
        install -D -m 644 "$license_file" \
            "%{buildroot}%{_licensedir}/open-mpv/dependencies/${crate_name}/$(basename "$license_file")"
    done
done
set -x

%check
shellcheck install.sh uninstall.sh
sh -n install.sh uninstall.sh
desktop-file-validate data/io.github.TheRealShek.OpenMpv.desktop
appstreamcli validate --no-net data/io.github.TheRealShek.OpenMpv.metainfo.xml
install -d -m 700 %{_builddir}/open-mpv-runtime
XDG_RUNTIME_DIR=%{_builddir}/open-mpv-runtime \
    dbus-run-session -- cargo test --frozen --offline

%files
%license %{_licensedir}/open-mpv
%doc README.md
%{_bindir}/open-mpv
%{_datadir}/applications/io.github.TheRealShek.OpenMpv.desktop
%{_datadir}/icons/hicolor/scalable/apps/io.github.TheRealShek.OpenMpv.svg
%{_datadir}/metainfo/io.github.TheRealShek.OpenMpv.metainfo.xml

%changelog
* Thu Aug 20 2026 therealshek <TheRealShek@users.noreply.github.com> - 0.1.0-1
- First packaged release
