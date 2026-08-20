#!/bin/sh
# Remove files created by the per-user source installation. RPM installations
# must be removed with the package manager instead.
set -eu

APP_ID="io.github.TheRealShek.OpenMpv"
LEGACY_APP_ID="dev.thakur.OpenMpv"
PREFIX="${HOME}/.local"

usage() {
    cat <<'EOF'
Usage: ./uninstall.sh [--prefix PATH]

Remove a source installation without changing the user's default applications.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            [ "$#" -ge 2 ] || { echo "--prefix requires a path" >&2; exit 2; }
            PREFIX=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$PREFIX" in
    /*) ;;
    *) echo "--prefix must be an absolute path" >&2; exit 2 ;;
esac

DATA_DIR="${PREFIX%/}/share"
APP_DIR="${DATA_DIR}/applications"
ICON_DIR="${DATA_DIR}/icons/hicolor/scalable/apps"

rm -f "${PREFIX%/}/bin/open-mpv" \
    "${APP_DIR}/${APP_ID}.desktop" \
    "${APP_DIR}/${LEGACY_APP_ID}.desktop" \
    "${ICON_DIR}/${APP_ID}.svg" \
    "${ICON_DIR}/${LEGACY_APP_ID}.svg" \
    "${DATA_DIR}/metainfo/${APP_ID}.metainfo.xml" \
    "${DATA_DIR}/licenses/open-mpv/LICENSE"
rmdir "${DATA_DIR}/licenses/open-mpv" 2>/dev/null || true
update-desktop-database "${APP_DIR}" 2>/dev/null || true
gtk-update-icon-cache -q "${DATA_DIR}/icons/hicolor" 2>/dev/null || true

echo "Removed open-mpv. Default applications were unchanged."
