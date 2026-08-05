# open-mpv — Product Plan

## What it is

A personal photo viewer for Linux (GNOME/Wayland), built in the spirit of mpv:
one thing done well, no chrome, no ceremony. It opens an image instantly,
lets you flip through the folder, zoom, rotate, and trash — and stays out
of the way. It is a **viewer**, not a photo manager: no library, no
database, no indexing.

## Who it is for

A single user (the author) on Fedora Workstation, GNOME on Wayland,
replacing Loupe as the system default image viewer.

## Product principles

1. **Instant.** The image is on screen before you notice the app launching.
2. **Invisible.** Frameless window, just the image. Controls exist but only
   appear when the mouse moves, then fade away.
3. **Both hands welcome.** Mouse and keyboard are equally first-class;
   neither is required.
4. **mpv-style configuration.** A plain-text config file, sane defaults,
   nothing to configure unless you want to.
5. **Never surprising with files.** The only writes it ever performs are the
   two the user explicitly asks for: move-to-trash and rotate-and-save.
   Trash is always recoverable; saves never corrupt the original.

## Scope

### In scope (this is the full product — no v0/v1 split)

- View a single image; flip through all images in its folder.
- Formats: JPEG, PNG, WebP, AVIF, BMP; animated GIF/WebP/PNG; SVG.
- Zoom, pan, fit modes, view rotation.
- Delete to trash with undo toast; rotate + save to disk.
- Frameless window with fade-in overlay controls; fullscreen.
- Single instance — opening another image reuses the window.
- mpv-style config file (`~/.config/open-mpv/open-mpv.conf`).
- Desktop integration: `.desktop` entry + MIME registration as the
  system default image viewer.

### Out of scope (deliberately)

- File explorer / thumbnail grid — planned for a later iteration; the
  architecture must not preclude it, but nothing is built now.
- Library management, tagging, search, cloud anything.
- Editing beyond rotate (crop, color, filters).
- EXIF panel, slideshow, clipboard copy (explicitly cut this iteration).
- Camera RAW.
- Non-Linux platforms.

## Technical approach

- **Language/toolkit: Rust + GTK4.** Native Wayland client, small memory
  footprint, no Electron/web runtime. Rust for reliability in the one
  place it matters here (file operations, async decode).
- **Image loading: glycin** (the sandboxed loader library Loupe uses) —
  gives JPEG/PNG/WebP/AVIF/GIF/SVG including animation, with decoders
  sandboxed away from the app process.
- **Single instance** via `GtkApplication` uniqueness (D-Bus activation);
  a second launch forwards its file argument to the running window.
- **Trash** via GIO's trash API (proper freedesktop trash, restorable);
  undo restores from trash.
- **Rotate + save:** JPEG rotation is lossless (EXIF orientation / lossless
  transform); other raster formats re-encode; SVG rotation is view-only
  (never rewritten). All saves are atomic (write temp, fsync, rename).
- **Rendering** through GTK4's GPU scene graph; large images decoded off
  the main thread so the UI never freezes.

## Future direction (not in scope, informs design)

The later "explorer" iteration will add a way to browse folders visually.
To keep that door open, the current design separates the *folder model*
(the sorted list of images in a directory, file watching, navigation)
from the *viewer surface* (the widget that displays one image). The
explorer will reuse the folder model and add a grid surface next to the
viewer surface — no rewrite required.
