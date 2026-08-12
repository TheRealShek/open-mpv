# open-mpv

A minimalist, mpv-inspired photo viewer for GNOME on Wayland. One
frameless window, just the image; controls fade in when the mouse
moves and get out of the way when it stops.

Built in Rust on GTK4, with all image decoding sandboxed through
[glycin](https://gitlab.gnome.org/GNOME/glycin) — the loader stack
GNOME's own viewer uses. No library, no database, no network, no
daemon. See [docs/PLAN.md](docs/PLAN.md) and
[docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) for the full spec.

## Features

- Every format the installed glycin loaders decode: JPEG, PNG, WebP,
  AVIF, BMP, HEIF/HEIC, JPEG XL, TIFF, JPEG 2000, ICO, TGA, QOI, EXR,
  DDS, PNM, XBM/XPM; animated GIF/WebP/APNG; SVG (re-rendered sharply at
  any zoom).
- Video (MP4, MKV, WebM, MOV, AVI) inline in the same folder flow:
  hardware-decoded (VA-API), looped, with pause/seek/volume and a seek
  bar in the overlay. Codec support is the system's GStreamer set.
- Flip through the folder of the opened image, natural filename order,
  live updates when files appear or vanish.
- Fit / 100% / free zoom anchored at the cursor; pixel-exact at 100%
  under fractional scaling. Arrow keys pan once you are zoomed in.
- The filename, position and zoom live in the fade-in overlay; the
  pointer fades with it, mpv-style.
- Delete to trash instantly, with an Undo toast. Rotate and save —
  lossless metadata-only for JPEG, atomic rewrite otherwise.
- Single instance: opening another image reuses the window.
- `?` shows the key cheat sheet.

## Install (Fedora / GNOME)

```sh
sudo dnf install gtk4-devel glycin-devel gstreamer1-devel   # build deps; runtime is stock
./install.sh                               # user install + default image/video viewer
./uninstall.sh                             # revert to Loupe (images) and mpv (videos)
```

## Keys (defaults)

| Key | Action |
| --- | ------ |
| Right / Page Down | next image (Right pans when zoomed in) |
| space | pause video · next image otherwise |
| Left / Backspace / Page Up | previous image (Left pans when zoomed in) |
| j / l | video seek −5 s / +5 s |
| m | video mute |
| Up / Down | video volume (pans when zoomed in) |
| Home / End | first / last image |
| scroll | zoom at cursor |
| horizontal scroll | navigate |
| + / − | zoom in / out |
| 0 / 1 / z | fit / 100% / toggle |
| r / R | rotate view right / left |
| s | save rotation to file |
| Delete | move to trash |
| Ctrl+Z | undo trash (while toast shows) |
| f / F11 / double-click | fullscreen |
| middle-click | fit / 100% toggle |
| q | quit |
| Escape | leave fullscreen, then quit |
| ? | key cheat sheet |

## Configuration

Optional, mpv-style: `~/.config/open-mpv/open-mpv.conf`.

```ini
# defaults shown
background = #121212
sort = name            # name | date (newest first)
sort-reverse = no      # flip whichever order `sort` picked
wrap = no              # wrap around at folder ends
fit = fit              # fit | actual — zoom when an image opens
overlay-timeout = 2.0  # seconds before controls fade out
hide-cursor = yes      # pointer fades with the overlay controls
start-fullscreen = no  # open fullscreen instead of sized to the media
loop = yes             # replay video at end of stream
volume = 100           # starting playback volume, 0-150 %
cache-budget-mb = 256  # decoded frames kept beyond the shown image
                       # (preloaded neighbors); lower it to trade RAM
                       # for a short decode wait on next/prev with
                       # very large photos

# rebind keys: bind = <key> <action>
# actions: right left up down next prev first last zoom-in zoom-out
#          zoom-fit zoom-actual zoom-toggle rotate-cw rotate-ccw
#          play-pause seek-back seek-forward mute volume-up volume-down
#          save trash undo fullscreen close help
# right/left/up/down are the contextual arrow actions: they pan a
# zoomed image and otherwise navigate or change volume.
bind = n next
bind = <Shift>d trash
bind = q none          # `none` removes a default binding outright
```

## Logs

Diagnostic logging is on by default. The trace on stderr records what
was opened, every decode with dimensions and duration, cache hits,
preloads, trash/restore/save results, and the cold-start time to first
frame. Set `OPEN_MPV_LOG=0` to disable it:

```sh
OPEN_MPV_LOG=0 open-mpv ~/Pictures
```

When launched from Files (desktop), stderr goes to the journal:

```sh
journalctl --user -t open-mpv -b   # or: journalctl --user -g open-mpv
```

Disabled logging is free (a single flag check per site — nothing is
formatted or written) and logging never runs on the frame-rendering path.
Genuine errors (failed decode, failed trash/save) always print to
stderr, log enabled or not.

## Development

```sh
cargo run -- <image-or-folder>
cargo test && cargo clippy -- -D warnings && cargo fmt
```
