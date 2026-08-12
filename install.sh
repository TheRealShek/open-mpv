#!/bin/sh
# Install open-mpv for the current user and register it as the default
# image viewer (FR-9). Re-run after changes; run uninstall.sh to revert.
set -eu

APP_ID="io.github.TheRealShek.OpenMpv"
LEGACY_APP_ID="dev.thakur.OpenMpv"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"
SRC_DIR="$(cd "$(dirname "$0")" && pwd)"

cargo build --release --manifest-path "${SRC_DIR}/Cargo.toml"

mkdir -p "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}"
install -m 755 "${SRC_DIR}/target/release/open-mpv" "${BIN_DIR}/open-mpv"
install -m 644 "${SRC_DIR}/data/${APP_ID}.svg" "${ICON_DIR}/${APP_ID}.svg"
sed "s|@BINDIR@|${BIN_DIR}|" "${SRC_DIR}/data/${APP_ID}.desktop" \
    > "${APP_DIR}/${APP_ID}.desktop"

# Remove the pre-release application ID so upgrading an existing source
# install does not leave a duplicate launcher behind.
rm -f "${APP_DIR}/${LEGACY_APP_ID}.desktop" \
    "${ICON_DIR}/${LEGACY_APP_ID}.svg"

update-desktop-database "${APP_DIR}" 2>/dev/null || true
gtk-update-icon-cache -q "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true

# Become the default viewer for every supported format (FR-9.1). The
# image list mirrors what the installed glycin loaders decode (see
# config::IMAGE_EXTENSIONS, pinned by a test); the video list mirrors
# config::VIDEO_EXTENSIONS. .m4v resolves to video/mp4, and
# video/x-msvideo is registered alongside the canonical video/vnd.avi
# because either name may reach us depending on the caller.
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

echo "Installed ${BIN_DIR}/open-mpv and registered as default image and video viewer."
