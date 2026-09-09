#!/bin/sh
# Shared preflight for source scripts; never query live paths for a staged install.

check_source_ownership() {
    if ! command -v rpm >/dev/null 2>&1; then
        echo "Cannot check package ownership: install Fedora's rpm tooling before changing a source installation." >&2
        exit 1
    fi
    if ! rpm -qa >/dev/null; then
        echo "Cannot read the RPM database; repair it before changing a source installation." >&2
        exit 1
    fi
    for source_path in \
        "$1/open-mpv" \
        "$2/applications/io.github.TheRealShek.OpenMpv.desktop" \
        "$2/applications/dev.thakur.OpenMpv.desktop" \
        "$2/icons/hicolor/scalable/apps/io.github.TheRealShek.OpenMpv.svg" \
        "$2/icons/hicolor/scalable/apps/dev.thakur.OpenMpv.svg" \
        "$2/metainfo/io.github.TheRealShek.OpenMpv.metainfo.xml" \
        "$2/licenses/open-mpv/LICENSE" \
        "$2/licenses/open-mpv" \
        "$2/applications/mimeinfo.cache" \
        "$2/icons/hicolor/icon-theme.cache"; do
        # Resolve parent aliases and also check a final symlink's target: tools
        # differ in whether they replace a destination link or follow it.
        source_parent=$(realpath -m -- "$(dirname -- "$source_path")")
        source_entry="${source_parent}/$(basename -- "$source_path")"
        source_target=$(realpath -m -- "$source_path")
        for source_candidate in "$source_entry" "$source_target"; do
            if rpm -qf -- "$source_candidate" >/dev/null 2>&1; then
                echo "Refusing to change RPM-owned path: $source_candidate" >&2
                echo "Use DNF to install, reinstall or remove the owning package; choose another --prefix for a source installation." >&2
                exit 1
            fi
        done
    done
}
