# open-mpv

open-mpv is a fast, simple photo and video viewer for GNOME. Open a file and
browse the photos and videos in its folder, all in one window. Move the
pointer when you need the controls; they stay out of the way while you view.

Your files stay where they are. There is nothing to import, no media library
to manage, and no tracking or network access.

<p align="center">
  <img
    src="docs/assets/open-mpv.webp"
    alt="open-mpv showing a photo with its controls visible"
    width="760"
  >
</p>

## What you can do

- View photos, animated images, SVGs and videos.
- Zoom in, move around an image, rotate the view or go fullscreen.
- Pause videos, seek, change speed and volume, and choose audio or subtitles.
- Move unwanted files to trash and use Undo to bring them back shortly after.
- Save a rotation in supported image formats. JPEG rotations keep the original
  image quality.
- Use **Quick Markup** to draw boxes and arrows on a still image, then copy the
  result to the clipboard.

Viewing and Quick Markup leave the original file unchanged. Files change only
when you choose to save a rotation, move them to trash or restore them.

## Install

Currently supported on **Fedora 44 Workstation, GNOME, Wayland and x86-64**.
Other distributions and desktops are not supported yet.

Previously installed from source? Follow the [source-to-RPM migration guide](docs/DISTRIBUTION.md#migrate-a-source-installation-to-rpm) first so an old binary or desktop launcher does not hide the RPM.

Install the latest release:

```sh
sudo dnf install https://github.com/TheRealShek/open-mpv/releases/latest/download/open-mpv-fedora44-x86_64.rpm
```

To update, run the same command when a new release is available. Updates are
manual: the usual `dnf upgrade` command will not find new GitHub releases.

To uninstall:

```sh
sudo dnf remove open-mpv
```

Installing open-mpv leaves your default apps unchanged. To use it as the
default for a file type, right-click a file in Files, choose **Open With**,
then select open-mpv as the default.

## Open a photo or video

Start open-mpv and choose **Open File** or **Open Folder**. You can also open a
file with open-mpv from Files, or drag one into the window.

Opening a file lets you browse the other supported photos and videos in the
same folder. Opening a folder starts with its first supported file.
The window starts at a size that fits your display and keeps that size as
you browse. Rapid navigation keeps image-loading work bounded and gives the
current photo priority over preloading neighbors.

If you prefer the terminal:

```sh
open-mpv ~/Pictures/photo.jpg
open-mpv ~/Videos/video.mp4
open-mpv ~/Pictures
```

## Controls

Move the pointer to show the controls. Right-click the photo or video, or
click the three-dot button, for more options. Press `?` for all shortcuts.
Hold the scroll-wheel (middle) button and drag to move the window at any
zoom. A middle-click without dragging switches between fit and actual size.
Left-drag pans when the image or video extends beyond the window; it does
nothing when the media fits. A grab cursor shows when panning is available and changes
to grabbing during a pan. Panning stops at the media edges; drag back to move
again. Window borders still resize, and Quick Markup uses left-drag to draw.

| Key or gesture | What it does |
| --- | --- |
| `Ctrl+O` / `Ctrl+Shift+O` | Open a file / folder |
| `Right` / `Left` | Next / previous file; move around the image when zoomed in |
| Scroll / pinch | Zoom in or out |
| Left-drag | Pan overflowing media; draw in Quick Markup |
| Middle-drag / middle-click | Move the window / switch fit and actual size |
| `0` / `1` / `Z` | Fit to the window / actual size / switch between them |
| `R` / `Shift+R` | Rotate right / left |
| `S` | Save the rotation, if the format supports it |
| `Delete` | Move the file to trash |
| `Ctrl+Z` | Undo a markup change or restore the file while Undo is available |
| `Space` | Pause or resume a video or animation; next file from a still image |
| `J` / `L` | Go back / forward 10 seconds in a video |
| `A` | Start or cancel Quick Markup |
| `F` / `F11` / double-click | Enter or leave fullscreen |
| `Escape` | Cancel the current mode, leave fullscreen or quit |

## File formats

**Images:** JPEG, PNG, WebP, AVIF, HEIF/HEIC, JPEG XL, TIFF, SVG, GIF and more,
depending on the installed image loaders. Animated GIF, WebP and PNG files
play automatically and loop. Press `Space` or the play/pause button to pause
an animation and resume from the same frame.

**Videos:** MP4, MKV, WebM, MOV and AVI. Playback depends on the codecs installed
on your system. open-mpv uses hardware decoding when compatible hardware and
drivers are available.

## Make it yours

You do not need a configuration file to get started. To change defaults or
keyboard shortcuts, follow the [configuration guide](docs/CONFIGURATION.md).

## Having trouble?

If images do not open, check that `glycin-loaders` is installed. If a video
will not play, you may need additional codecs. The optional
`gstreamer1-plugin-libav` package adds software decoding for more video formats.

The [troubleshooting guide](docs/TROUBLESHOOTING.md) explains how to check
video support, understand configuration failures, and capture crash backtraces. If you opened the app from
Files, you can read its logs with:

```sh
journalctl -b _COMM=open-mpv
```

## Contributing

Want to build from source or help with development? See
[CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE).
