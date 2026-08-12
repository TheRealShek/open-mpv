# open-mpv — Product Plan

## What it is

A personal local-media viewer for Linux, built in the spirit of mpv: fast,
minimal and configurable without becoming a library, editor or general media
player. [REQUIREMENTS.md](REQUIREMENTS.md) defines its complete behavior; this
document defines why the product exists and where its scope stops.

## Who it is for

The author, on Fedora Workstation with GNOME/Wayland, replacing Loupe as the
default image viewer.

## Product principles

1. **Instant.** Show media before launch is noticed.
2. **Invisible.** Keep the window frameless and controls contextual.
3. **Both hands welcome.** Mouse and keyboard are equally first-class.
4. **mpv-style configuration.** Use sane defaults and one optional text file.
5. **Never surprising with files.** The only writes it ever performs are the
   explicit trash/restore and rotate-save operations. Trash is recoverable and
   saves are atomic.

## Scope

### In scope

- Open a local image, video or folder and navigate supported media in that
  folder through one viewer surface.
- View still, animated and vector images; play local video with focused
  transport and local subtitle support.
- Zoom, pan, fit, view-rotate and use a frameless contextual interface through
  mouse, touchpad or configurable keyboard actions.
- Trash with undo and explicitly save supported image rotations. These are the
  only persistent file operations.
- Optional mpv-style configuration, single-instance activation and GNOME
  desktop integration.

This is the full current product, not a staged v0/v1 list. Exact formats,
controls and acceptance criteria belong only in [REQUIREMENTS.md](REQUIREMENTS.md).

### Out of scope (deliberately)

- File explorer / thumbnail grid; architecture may prepare for it, but the UI
  is a later iteration.
- Library management, indexing, tagging, ratings, search or cloud features.
- Editing beyond rotate (crop, color, filters).
- EXIF panels, slideshows, clipboard copy, export and screenshots.
- Network or online playback, telemetry, background services and an in-app
  updater.
- General media-player growth such as playlists.
- Audio track selection, subtitle downloading/search, dual subtitles,
  subtitle editing, and timing or style controls. Subtitle display stays a
  focused viewing aid; those controls would turn it into a media player.
- Camera RAW until glycin provides a loader; this is a decoder limitation, not
  a product-policy exclusion.
- Non-Linux platforms.

## Future direction (not in scope, informs design)

The later "explorer" iteration will add a way to browse folders visually.
The current folder model must therefore remain reusable by another UI surface;
that architectural constraint is specified by NFR-6.1 and enforced in
[../AGENTS.md](../AGENTS.md).
