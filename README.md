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

- JPEG, PNG, WebP, AVIF, BMP; animated GIF/WebP/APNG; SVG (re-rendered
  sharply at any zoom).
- Flip through the folder of the opened image, natural filename order,
  live updates when files appear or vanish.
- Fit / 100% / free zoom anchored at the cursor; pixel-exact at 100%
  under fractional scaling.
- Delete to trash instantly, with an Undo toast. Rotate and save —
  lossless metadata-only for JPEG, atomic rewrite otherwise.
- Single instance: opening another image reuses the window.
- `?` shows the key cheat sheet.

## Install (Fedora / GNOME)

```sh
sudo dnf install gtk4-devel glycin-devel   # build deps; runtime is stock
./install.sh                               # user install + default viewer
./uninstall.sh                             # revert to Loupe
```

## Keys (defaults)

| Key | Action |
| --- | ------ |
| Right / space / Page Down | next image |
| Left / Backspace / Page Up | previous image |
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
wrap = no              # wrap around at folder ends
fit = fit              # fit | actual — zoom when an image opens
overlay-timeout = 2.0  # seconds before controls fade out

# rebind keys: bind = <key> <action>
# actions: next prev first last zoom-in zoom-out zoom-fit zoom-actual
#          zoom-toggle rotate-cw rotate-ccw save trash undo fullscreen
#          close help
bind = n next
bind = <Shift>d trash
```

## Logs

Silent by default. Set `OPEN_MPV_LOG=1` to get a diagnostic trace on
stderr — what was opened, every decode with dimensions and duration,
cache hits, preloads, trash/restore/save results, and the cold-start
time to first frame:

```sh
OPEN_MPV_LOG=1 open-mpv ~/Pictures | grep -v Gsk
```

When launched from Files (desktop), stderr goes to the journal:

```sh
journalctl --user -t open-mpv -b   # or: journalctl --user -g open-mpv
```

Logging is free when off (a single flag check per site — nothing is
formatted or written) and never runs on the frame-rendering path.
Genuine errors (failed decode, failed trash/save) always print to
stderr, log enabled or not.

## Development

```sh
cargo run -- <image-or-folder>
cargo test && cargo clippy -- -D warnings && cargo fmt
```
