#!/bin/sh
# Remove open-mpv and hand default-viewer status back to Loupe for
# images and to mpv for videos.
set -eu

APP_ID="io.github.TheRealShek.OpenMpv"
LEGACY_APP_ID="dev.thakur.OpenMpv"
DESKTOP="${HOME}/.local/share/applications/${APP_ID}.desktop"
LEGACY_DESKTOP="${HOME}/.local/share/applications/${LEGACY_APP_ID}.desktop"

# Read the associations back out of the entry before deleting it, rather
# than keeping a third copy of the MIME list in sync by hand. Anything
# install.sh claimed is handed back here by construction; images go to
# Loupe, which decodes them through the same glycin loaders we do.
MIMES=$(
    {
        sed -n 's/^MimeType=//p' "${DESKTOP}" 2>/dev/null || true
        sed -n 's/^MimeType=//p' "${LEGACY_DESKTOP}" 2>/dev/null || true
    } | tr ';' ' '
)

rm -f "${HOME}/.local/bin/open-mpv" "${DESKTOP}" "${LEGACY_DESKTOP}" \
    "${HOME}/.local/share/icons/hicolor/scalable/apps/${APP_ID}.svg" \
    "${HOME}/.local/share/icons/hicolor/scalable/apps/${LEGACY_APP_ID}.svg"
update-desktop-database "${HOME}/.local/share/applications" 2>/dev/null || true

for mime in ${MIMES}; do
    case "$mime" in
        image/*) xdg-mime default org.gnome.Loupe.desktop "$mime" 2>/dev/null || true ;;
        video/*) xdg-mime default mpv.desktop "$mime" 2>/dev/null || true ;;
    esac
done

echo "Removed open-mpv; Loupe handles images and mpv handles videos again."
