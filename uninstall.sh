#!/bin/sh
# Remove open-mpv and hand default-viewer status back to Loupe for
# images and to mpv for videos.
set -eu

APP_ID="dev.thakur.OpenMpv"
rm -f "${HOME}/.local/bin/open-mpv" \
    "${HOME}/.local/share/applications/${APP_ID}.desktop" \
    "${HOME}/.local/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
update-desktop-database "${HOME}/.local/share/applications" 2>/dev/null || true

for mime in image/jpeg image/png image/webp image/avif image/bmp \
    image/gif image/svg+xml image/svg+xml-compressed; do
    xdg-mime default org.gnome.Loupe.desktop "$mime" 2>/dev/null || true
done

for mime in video/mp4 video/x-matroska video/webm video/quicktime \
    video/vnd.avi video/x-msvideo; do
    xdg-mime default mpv.desktop "$mime" 2>/dev/null || true
done

echo "Removed open-mpv; Loupe handles images and mpv handles videos again."
