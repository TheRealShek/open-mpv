# open-mpv

open-mpv is a fast, minimal photo and video viewer for GNOME on Wayland. It
opens local media without a library, database or network connection. Its
controls stay out of the way until you need them.

> **Current support:** Fedora 44 Workstation, GNOME, Wayland and x86-64. Other
> Linux distributions and desktop environments are not supported yet.

![open-mpv showing a photo with its controls visible](docs/assets/open-mpv.webp)

## Install on Fedora 44

There is no published package yet. Install the current version for your user
account from source:

```sh
sudo dnf install cargo desktop-file-utils gcc git glycin-devel glycin-loaders \
  gstreamer1-devel gstreamer1-plugin-gtk4 gstreamer1-plugins-base \
  gstreamer1-plugins-good gtk4-devel rust xdg-utils
git clone https://github.com/TheRealShek/open-mpv.git
cd open-mpv
./install.sh
```

The installer writes under `~/.local` and does not change your default apps.

Optional: make open-mpv the default for every supported photo and video type:

```sh
./install.sh --set-default
```

To change only one file type, right-click that type of file in Files, choose
**Open With**, then select open-mpv.

To update an existing source installation:

```sh
cd open-mpv
git pull --ff-only
./install.sh
```

To remove it, run this from the same checkout:

```sh
./uninstall.sh
```

## Open a photo, video or folder

Open media from Files, drag a file into the window, or use the command line:

```sh
open-mpv ~/Pictures/photo.jpg
open-mpv ~/Videos/video.mp4
open-mpv ~/Pictures
```

You can also start open-mpv with no path and choose **Open File** or
**Open Folder**.

## What it does

- Opens photos, animated images, SVG files and local videos in one window.
- Lets you move through every supported media file in the current folder.
- Supports zoom, pan, fit, rotation and fullscreen.
- Plays local videos with seeking, volume, speed and subtitle controls.
- Moves files to trash and offers a short Undo action.
- Saves supported image rotations. JPEG rotation is lossless.
- Lets you draw a box or arrow on a still image and copy the result without
  changing the original file.
- Supports optional mpv-style configuration and custom key bindings.

## Privacy and file safety

- open-mpv has no network access, telemetry or persistent background service.
- glycin decodes images in a separate sandboxed process.
- The app writes only when you trash, restore or explicitly save a rotation.
- Quick Markup copies an image to the clipboard. It never changes or creates a
  media file.

## Supported media

Images include JPEG, PNG, WebP, AVIF, HEIF/HEIC, JPEG XL, TIFF, SVG, GIF and
other formats supported by the installed glycin loaders. Animated GIF, WebP
and PNG files play automatically.

Videos include MP4, MKV, WebM, MOV and AVI. Playback uses the system GStreamer
codecs and hardware decoding when available. The optional
`gstreamer1-plugin-libav` package adds a software fallback for more videos.
Pitch-preserving playback speed needs `gstreamer1-plugins-good`.

## Main controls

Press `?` inside the app for the complete shortcut guide.

| Key or gesture | Action |
| --- | --- |
| `Ctrl+O` | Open a file |
| `Ctrl+Shift+O` | Open a folder |
| `Right` / `Page Down` | Next file; pan right when zoomed |
| `Left` / `Page Up` | Previous file; pan left when zoomed |
| Scroll / pinch | Zoom at the pointer |
| Horizontal scroll | Previous or next file |
| `0` / `1` / `Z` | Fit / actual size / toggle between them |
| `R` / `Shift+R` | Rotate right / left |
| `S` | Save the current rotation when supported |
| `Delete` | Move the current file to trash |
| `Ctrl+Z` | Undo Quick Markup or the latest offered trash action |
| `Space` | Pause or resume video; move to the next still image |
| `J` / `L` | Seek video back / forward 10 seconds |
| `[` / `]` / `\` | Slower / faster / normal video speed |
| `V` / `Shift+V` | Show or hide / cycle subtitles |
| `A` | Start or cancel Quick Markup |
| `B` / `Shift+A` | Choose the box / arrow markup tool |
| `Ctrl+C` | Copy the marked-up image |
| `F` / `F11` / double-click | Toggle fullscreen |
| `Q` | Quit |
| `Escape` | Cancel the current mode, leave fullscreen or quit |

Move the pointer to show controls. Right-click the media or use the three-dot
button for less common actions.

## Configuration

Configuration is optional. Create `~/.config/open-mpv/open-mpv.conf` only when
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

Unknown or invalid settings produce a warning but do not stop the app from
opening.

## Troubleshooting

If images do not open, check that `glycin-loaders` is installed. Video support
depends on the installed GStreamer plugins and codecs.

open-mpv writes diagnostic messages to stderr. When it was opened from Files,
view them with:

```sh
journalctl -b _COMM=open-mpv
```

Add `-f` to follow the log while reproducing a problem. Set `OPEN_MPV_LOG=0`
to hide routine diagnostics; errors are still reported.

## Development

After following the installation steps above, these are the main development
commands:

```sh
cargo run -- <file-or-folder>
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

## Project documents

- [Requirements](docs/REQUIREMENTS.md) define the exact product behavior and
  performance limits.
- [Product plan](docs/PLAN.md) explains the product goal and what is out of
  scope.
- [Distribution](docs/DISTRIBUTION.md) explains packaging and release choices.

## License

open-mpv is available under the [MIT License](LICENSE).
