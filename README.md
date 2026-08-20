# open-mpv

open-mpv is a fast, distraction-free photo and video viewer for GNOME on
Wayland. Open a file and the window gets out of the way: no library to set up,
no database, and no toolbar covering your picture.

It takes its cues from [mpv](https://mpv.io/): simple by default, quick from
the keyboard, and configurable with a plain-text file when you want it to be.

> open-mpv currently targets Fedora 44 Workstation on x86-64 with GNOME and
> Wayland. Packaged releases use GitHub RPM assets; other Linux
> distributions are not currently supported. See
> [Distribution and updates](docs/DISTRIBUTION.md) for the release model.

![open-mpv displaying a landscape with its overlay controls visible](docs/assets/open-mpv.webp)

## What you can do

- Open files or folders in the app and browse supported photos and videos in
  one folder-navigation flow.
- Zoom, pan, fit, rotate and use fullscreen with mouse, touchpad or keyboard.
- Draw a quick box or arrow on a still image and copy the annotated result for
  pasting elsewhere, without changing the original file.
- Trash files with undo and save supported image rotations losslessly for JPEG.
- Control local video speed and subtitles, with optional mpv-style
  configuration.

Images are sandbox-decoded through
[glycin](https://gitlab.gnome.org/GNOME/glycin). The app has no network,
telemetry, media library or background service.

## Supported files

open-mpv supports JPEG, PNG, WebP, AVIF, HEIF/HEIC, JPEG XL, TIFF, SVG, GIF,
and many other image formats provided by the installed glycin loaders.
Animated GIF, WebP, and PNG files play automatically.

MP4, MKV, WebM, MOV, and AVI videos play through the system's GStreamer
codecs. Hardware decoding is used when available. The RPM recommends
`gstreamer1-plugins-bad-free` for the preferred Intel QSV hardware decoders.
The optional `gstreamer1-plugin-libav` package provides a software fallback
for some videos. Playback-speed audio keeps its pitch through `scaletempo`
from the `gstreamer1-plugins-good` package; without it, videos remain at 1×.

## Install on Fedora 44

After the first RPM appears on the
[Releases page](https://github.com/TheRealShek/open-mpv/releases), install or
update the latest x86-64 release directly from GitHub:

```sh
sudo dnf install \
  https://github.com/TheRealShek/open-mpv/releases/latest/download/open-mpv-fedora44-x86_64.rpm
```

Until that first release is approved and published, the URL returns 404; use
the source installation under [Development](#development) instead.

DNF downloads the RPM, resolves its Fedora dependencies and tracks every
installed file. The package does not change default applications. Choose
open-mpv through Files' **Open With** dialog if you want it to handle a media
type by default.

GitHub is not a DNF repository, so `dnf upgrade` cannot discover a new
open-mpv release. Re-run the command above when a release is announced; DNF
will upgrade the installed package. Remove it with:

```sh
sudo dnf remove open-mpv
```

After installation, open a supported file from Files or run:

```sh
open-mpv ~/Pictures/photo.jpg
open-mpv ~/Pictures
```

You can also launch open-mpv with no file, then use its Open File or Open
Folder controls or drag a file into the window. File and folder opening remain
available from the More menu while viewing media.

## Everyday controls

Press `?` inside the app to see the complete shortcut guide.

Moving the pointer reveals file information, navigation and media-specific
controls. Less frequent actions live in the More menu, available from its
three-dot button or by right-clicking the media.

| Key or gesture | Action |
| --- | --- |
| `Ctrl+O` | Open a supported image or video |
| `Ctrl+Shift+O` | Open a folder at its first supported item |
| `Right` / `Page Down` | Next file; `Right` pans a zoomed image |
| `Left` / `Page Up` | Previous file; `Left` pans a zoomed image |
| Scroll / pinch | Zoom at the pointer |
| Horizontal scroll | Previous or next file |
| `0` / `1` / `Z` | Fit / actual size / toggle between them |
| `R` / `Shift+R` | Rotate right / left |
| `A` | Start or cancel Quick Markup on a static image |
| `B` / `Shift+A` | Select the box / arrow Quick Markup tool |
| `Ctrl+C` | Copy the annotated image and leave Quick Markup |
| `C` / `Ctrl+Z` | Clear all / undo the last Quick Markup change |
| `S` | Save the current rotation |
| `Delete` | Move the current file to trash |
| `Ctrl+Z` | Outside Quick Markup, undo the most recent trash action while offered |
| `Space` | Pause or resume video; next file for an image |
| `J` / `Shift+Left` | Seek video back 10 seconds |
| `L` / `Shift+Right` | Seek video forward 10 seconds |
| `[` / `]` / `\` | Decrease / increase / reset video playback speed |
| `M` | Mute video |
| `V` / `Shift+V` | Show or hide subtitles / cycle subtitle tracks |
| `F` / `F11` / double-click | Toggle fullscreen |
| `Q` | Quit |
| `Escape` | Cancel Quick Markup, leave fullscreen, then quit |

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
bind = bracketright speed-up
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

Install the complete Fedora build and runtime stack before building from
source:

```sh
sudo dnf install cargo desktop-file-utils gcc glycin-devel glycin-loaders \
  gstreamer1-devel gstreamer1-plugin-gtk4 gstreamer1-plugins-base \
  gstreamer1-plugins-good gtk4-devel rust xdg-utils
git clone https://github.com/TheRealShek/open-mpv.git
cd open-mpv
./install.sh
```

The source installer writes under `~/.local` and leaves default applications
unchanged. Use `./install.sh --set-default` to opt into every supported image
and video association, and `./uninstall.sh` to remove the source installation.

```sh
cargo run -- <file-or-folder>
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

open-mpv is available under the [MIT License](LICENSE).
