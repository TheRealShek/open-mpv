# Troubleshooting open-mpv

## Images do not open

Check that `glycin-loaders` is installed. open-mpv uses the image loaders
available through glycin, so a missing loader can leave a format unavailable.

## A video does not play

Video support depends on the installed GStreamer plugins, graphics driver and
codecs. On Fedora, `gstreamer1-plugins-bad-free` supplies hardware-decoder
plugins. The optional `gstreamer1-plugin-libav` package supplies software
fallbacks for more formats, and pitch-preserving playback speed needs
`gstreamer1-plugins-good`.

The Reference environment is verified with Intel QSV. Other systems may expose
VA-API or NVIDIA NVDEC, but those paths are not currently supported claims.

Inspect the available H.264 decoders with:

```sh
gst-inspect-1.0 qsvh264dec
gst-inspect-1.0 vah264dec
gst-inspect-1.0 nvh264dec
gst-inspect-1.0 avdec_h264
```

A missing hardware decoder is normal when its backend or driver is not
available. A missing `avdec_h264` means the optional software fallback is not
installed.

## Read the diagnostics

open-mpv writes diagnostic messages to stderr. When it was opened from Files,
read them from the journal:

```sh
journalctl -b _COMM=open-mpv
```

Add `-f` to follow the log while reproducing a problem. Set `OPEN_MPV_LOG=0` to
hide routine diagnostics; errors are still reported.

For video, the diagnostics include the encoded stream and selected decoder,
including whether GStreamer classifies it as hardware or software. When
reporting a playback problem, include the relevant log lines, the media format
and the installed decoder reported by `gst-inspect-1.0`.
