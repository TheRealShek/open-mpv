#!/bin/sh
# Build and install open-mpv. Packaging can stage an existing release build
# with --no-build --prefix /usr --destdir <buildroot>.
set -eu

APP_ID="io.github.TheRealShek.OpenMpv"
LEGACY_APP_ID="dev.thakur.OpenMpv"
SRC_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PREFIX="${HOME}/.local"
DESTDIR=""
BUILD=true
SET_DEFAULT=false

usage() {
    cat <<'EOF'
Usage: ./install.sh [OPTIONS]

Options:
  --prefix PATH     Installation prefix (default: ~/.local)
  --destdir PATH    Stage files below PATH for packaging
  --no-build        Install the existing target/release/open-mpv binary
  --set-default     Make open-mpv the default for supported media
  -h, --help        Show this help
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            [ "$#" -ge 2 ] || { echo "--prefix requires a path" >&2; exit 2; }
            PREFIX=$2
            shift 2
            ;;
        --destdir)
            [ "$#" -ge 2 ] || { echo "--destdir requires a path" >&2; exit 2; }
            DESTDIR=$2
            shift 2
            ;;
        --no-build)
            BUILD=false
            shift
            ;;
        --set-default)
            SET_DEFAULT=true
            shift
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
case "$PREFIX" in
    *[!A-Za-z0-9_./-]*)
        echo "--prefix may contain only letters, digits, '_', '.', '/' and '-'" >&2
        exit 2
        ;;
esac
case "$DESTDIR" in
    ""|/*) ;;
    *) echo "--destdir must be an absolute path" >&2; exit 2 ;;
esac
if [ -n "$DESTDIR" ] && [ "$SET_DEFAULT" = true ]; then
    echo "--set-default cannot be used with a staged installation" >&2
    exit 2
fi

INSTALL_BIN_DIR="${PREFIX%/}/bin"
INSTALL_DATA_DIR="${PREFIX%/}/share"
BIN_DIR="${DESTDIR}${INSTALL_BIN_DIR}"
APP_DIR="${DESTDIR}${INSTALL_DATA_DIR}/applications"
ICON_DIR="${DESTDIR}${INSTALL_DATA_DIR}/icons/hicolor/scalable/apps"
METAINFO_DIR="${DESTDIR}${INSTALL_DATA_DIR}/metainfo"
LICENSE_DIR="${DESTDIR}${INSTALL_DATA_DIR}/licenses/open-mpv"

if [ "$BUILD" = true ]; then
    cargo build --release --locked --manifest-path "${SRC_DIR}/Cargo.toml"
fi

if [ ! -x "${SRC_DIR}/target/release/open-mpv" ]; then
    echo "target/release/open-mpv does not exist; build it or omit --no-build" >&2
    exit 1
fi

mkdir -p "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}" "${METAINFO_DIR}" "${LICENSE_DIR}"
install -m 755 "${SRC_DIR}/target/release/open-mpv" "${BIN_DIR}/open-mpv"
install -m 644 "${SRC_DIR}/data/${APP_ID}.svg" "${ICON_DIR}/${APP_ID}.svg"
install -m 644 "${SRC_DIR}/data/${APP_ID}.metainfo.xml" \
    "${METAINFO_DIR}/${APP_ID}.metainfo.xml"
install -m 644 "${SRC_DIR}/LICENSE" "${LICENSE_DIR}/LICENSE"
sed "s|@BINDIR@|${INSTALL_BIN_DIR}|" "${SRC_DIR}/data/${APP_ID}.desktop" \
    > "${APP_DIR}/${APP_ID}.desktop"
chmod 644 "${APP_DIR}/${APP_ID}.desktop"

# Remove the pre-release application ID so upgrading an existing source
# install does not leave a duplicate launcher behind.
rm -f "${APP_DIR}/${LEGACY_APP_ID}.desktop" \
    "${ICON_DIR}/${LEGACY_APP_ID}.svg"

if [ -z "$DESTDIR" ]; then
    update-desktop-database "${INSTALL_DATA_DIR}/applications" 2>/dev/null || true
    gtk-update-icon-cache -f -t -q "${INSTALL_DATA_DIR}/icons/hicolor" 2>/dev/null || true
fi

if [ "$SET_DEFAULT" = true ]; then
    # This list mirrors the desktop entry and config's supported extensions.
    for mime in image/jpeg image/png image/apng image/webp image/avif \
        image/bmp image/gif image/svg+xml image/svg+xml-compressed \
        image/heif image/jxl image/tiff image/jp2 image/x-jp2-codestream \
        image/vnd.microsoft.icon image/x-win-bitmap image/x-tga image/qoi \
        image/x-exr image/x-dds image/x-portable-anymap \
        image/x-portable-bitmap image/x-portable-graymap \
        image/x-portable-pixmap image/x-xbitmap image/x-xpixmap \
        video/mp4 video/x-matroska video/webm video/quicktime \
        video/vnd.avi video/x-msvideo; do
        xdg-mime default "${APP_ID}.desktop" "$mime"
    done
fi

echo "Installed ${BIN_DIR}/open-mpv."
if [ "$SET_DEFAULT" = false ] && [ -z "$DESTDIR" ]; then
    echo "Default applications were unchanged; use --set-default to select open-mpv for supported media."
fi
