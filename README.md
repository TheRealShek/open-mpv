# open-mpv

open-mpv is a fast, distraction-free photo and video viewer for GNOME on
Wayland. Open a file and the window gets out of the way: no library to set up,
no database, and no toolbar covering your picture.

It takes its cues from [mpv](https://mpv.io/): simple by default, quick from
the keyboard, and configurable with a plain-text file when you want it to be.

> open-mpv currently targets Fedora Workstation with GNOME on Wayland. It is
> usable today, but installation still requires building it from source. See
> [Distribution and updates](docs/DISTRIBUTION.md) for the path to packaged
> releases.

![open-mpv displaying a landscape with its overlay controls visible](docs/assets/open-mpv.webp)

## What you can do

- Browse supported photos and videos in one folder-navigation flow.
- Zoom, pan, fit, rotate and use fullscreen with mouse, touchpad or keyboard.
- Trash files with undo and save supported image rotations losslessly for JPEG.
- Control local video and subtitles, with optional mpv-style configuration.

Images are sandbox-decoded through
[glycin](https://gitlab.gnome.org/GNOME/glycin). The app has no network,
telemetry, media library or background service.

## Supported files

open-mpv supports JPEG, PNG, WebP, AVIF, HEIF/HEIC, JPEG XL, TIFF, SVG, GIF,
and many other image formats provided by the installed glycin loaders.
Animated GIF, WebP, and PNG files play automatically.

MP4, MKV, WebM, MOV, and AVI videos play through the system's GStreamer
codecs. Hardware decoding is used when available. The optional
`gstreamer1-plugin-libav` package provides a software fallback for some
videos.

## Install on Fedora

To install the current version for your user account:

```sh
git clone https://github.com/TheRealShek/open-mpv.git
cd open-mpv
sudo dnf install gtk4-devel glycin-devel gstreamer1-devel
sudo dnf install gstreamer1-plugin-libav # optional video fallback
./install.sh
```

The install script builds a release binary, installs it under `~/.local`, and
makes open-mpv the default viewer for supported photos and videos. Run
`./uninstall.sh` from the same checkout to remove it and restore Loupe and mpv
as the defaults.

After installation, open a supported file from Files or run:

```sh
open-mpv ~/Pictures/photo.jpg
open-mpv ~/Pictures
```

You can also launch open-mpv with no file and drag a file into its window.

## Everyday controls

Press `?` inside the app to see the complete shortcut guide.

Moving the pointer reveals file information, navigation and media-specific
controls. Less frequent actions live in the More menu, available from its
three-dot button or by right-clicking the media.

| Key or gesture | Action |
| --- | --- |
| `Right` / `Page Down` | Next file; `Right` pans a zoomed image |
| `Left` / `Page Up` | Previous file; `Left` pans a zoomed image |
| Scroll / pinch | Zoom at the pointer |
| Horizontal scroll | Previous or next file |
| `0` / `1` / `Z` | Fit / actual size / toggle between them |
| `R` / `Shift+R` | Rotate right / left |
| `S` | Save the current rotation |
| `Delete` | Move the current file to trash |
| `Ctrl+Z` | Undo the most recent trash action while offered |
| `Space` | Pause or resume video; next file for an image |
| `J` / `Shift+Left` | Seek video back 10 seconds |
| `L` / `Shift+Right` | Seek video forward 10 seconds |
| `M` | Mute video |
| `V` / `Shift+V` | Show or hide subtitles / cycle subtitle tracks |
| `F` / `F11` / double-click | Toggle fullscreen |
| `Q` | Quit |
| `Escape` | Leave fullscreen, then quit |

## Configuration

Configuration is optional. Create `~/.config/open-mpv/open-mpv.conf` only if
you want to change a default:

```ini
background = #121212
sort = name            # name | date
sort-reverse = no
wrap = no
fit = fit              # fit | actual
overlay-timeout = 2.0
hide-cursor = yes
start-fullscreen = no
loop = yes
volume = 100           # 0-150
subtitles = auto       # auto | off
cache-budget-mb = 256

# Add or replace key bindings.
bind = n next
bind = <Shift>d trash
bind = q none          # remove a default binding
```

Unknown or invalid settings are ignored with a warning, so a broken config
does not prevent the viewer from opening.

## Troubleshooting

open-mpv writes a diagnostic trace to stderr. When it was opened from Files,
view the log with:

```sh
journalctl -b _COMM=open-mpv
```

Add `-f` to follow a reproduction live. The `_COMM` filter includes both the
application trace and GTK/GStreamer diagnostics from the desktop launch.

Disable routine diagnostic logging with `OPEN_MPV_LOG=0`. Errors are still
reported.

## Development

Start with [AGENTS.md](AGENTS.md), which routes contributors to the
authoritative requirements, scope and distribution documents and records the
repository's engineering constraints.

```sh
cargo run -- <file-or-folder>
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

open-mpv is available under the [MIT License](LICENSE).
