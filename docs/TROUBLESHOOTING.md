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

## Image loading under rapid navigation

`loader: start` and `loader: finish` diagnostics report active and queued
first-frame jobs. Active counts include cancelled jobs until their futures
finish. The exact bounds are in [Performance and bounded work](REQUIREMENTS.md#nfr-1--performance-and-bounded-work).
These counts exclude retained animated-image/SVG loaders and Glycin's idle
process pool, so also measure processes and PSS when investigating growth.

From a source checkout in a GNOME/Wayland session, run the deterministic slow
loader regression separately from other desktop tests:

```sh
cargo test --locked window::decode_tests::rapid_navigation_bounds_slow_decodes -- --ignored --exact --nocapture
```

For real decoding, provide a disposable fixture folder with at least 12 valid
images (for example, 12 MP PNGs) and run:

```sh
OPEN_MPV_STRESS_DIR=/path/to/fixtures cargo test --release --locked window::decode_tests::sustained_real_decodes -- --ignored --exact --nocapture
```

This performs six rounds of 200 selections, 25 ms apart, with a three-second
settling period after each round. The log gives the test process PID. Sample
`Pss` in `/proc/<pid>/smaps_rollup` for that process and its descendants, and
count Glycin loader processes throughout navigation and settling. Compare
successive rounds; record image dimensions/formats, peak counts and PSS, and
settled values. A large image can require substantial decode memory even with
bounded job counts. This automated check does not replace human testing of
navigation, animation, SVG zoom, mixed video/image folders and quit.

## Configuration and folder changes

A missing optional configuration file uses defaults. If the file cannot be read
(including invalid UTF-8), stderr reports its path and the actual cause, then
uses defaults. Invalid settings warn and retain the previous value or default;
see [Configuration](CONFIGURATION.md) for the supported timeout range.

Folder-monitor creation and changed-file query failures include the operation,
path and cause in stderr. Cancellation and files disappearing during a query
are expected and stay quiet. If monitoring cannot start, reopen the folder to
refresh its contents after external changes.

## Unexpected crashes

For a reproducible panic, build the affected source revision with debug symbols:

```sh
cargo build --profile diagnostic --locked
RUST_BACKTRACE=1 ./target/diagnostic/open-mpv /path/to/media
```

Close any running open-mpv first so the single-instance request reaches this
build. The diagnostic profile keeps release optimization but retains Rust debug
information and symbols. A panic backtrace should include application function
names and source lines. Use `RUST_BACKTRACE=full` for the unabridged stack. Keep
the exact binary, source revision and log together; paths in logs may be private.

Ordinary returned errors do not unwind the stack. `RUST_BACKTRACE` does not add
backtraces to them: their typed causes and operation context are reported in
stderr. Expected configuration, decoder and file failures do not capture stacks.
Native crashes are different again; inspect a retained diagnostic build with
`coredumpctl debug` when the system captured a core. Native library frames may
also need the matching Fedora debuginfo packages.

See [Distribution](DISTRIBUTION.md#diagnostic-builds) for the symbol policy.
