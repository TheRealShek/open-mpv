# AGENTS.md

This file tells coding agents how to work safely in this repository.

open-mpv is a small local photo and video viewer for Fedora Workstation,
GNOME and Wayland. It uses Rust, GTK4, glycin, GStreamer and GIO.

## Work in this order

1. Understand the request. A request to explain, review or diagnose is
   read-only. A request to build, fix or change allows focused local edits.
2. Read the document that owns the decision:
   - [README.md](README.md) for user-facing features, installation and usage.
   - [CONTEXT.md](CONTEXT.md) for product language and boundaries.
   - [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) for exact behavior and
     performance limits. Read it before changing behavior.
   - [docs/DISTRIBUTION.md](docs/DISTRIBUTION.md) for packaging and releases.
3. Inspect the relevant module, its callers, its tests and similar code. Find
   the cause before changing anything.
4. Make the smallest complete change. Reuse an existing helper when it fits.
   Do not add unrelated cleanup or speculative abstractions.
5. Update product documents in the same change when behavior, scope or a
   packaging decision changes. Use requirement names such as FR-5.4 and
   NFR-1.1 when they help connect code to the specification.
6. Run checks that match the risk, then run the complete required checks when
   the change is ready.
7. If the built app or package changed and all relevant checks pass, install
   the verified build on the development system with the repository workflow.
   Never install a failed or unresolved build.
8. Report why the change was needed, what changed, what passed and anything
   still requiring human testing.

Do not commit, push, open a pull request or merge unless the user asks. Never
overwrite unrelated work in a dirty worktree.

## Rules that must not change

- Source media changes remain explicit and belong in `fileops`. The other
  approved persistent writes are atomic Preferences updates to the canonical
  config and standard freedesktop thumbnails for Explorer. Do not add private
  caches, history, state files or implicit source writes without a product
  decision. Quick Markup remains clipboard-only.
- Do not add a dependency without explaining why existing Rust, GTK, GIO,
  glycin or GStreamer tools are not enough. Natural sort, `key=value` parsing
  and CLI handling are intentionally implemented in this repository.
- Panics are bugs. Broken media, unreadable paths, missing codecs, invalid
  configuration and failed file operations are normal errors. Show the
  specified in-window or stderr error instead of crashing.
- The NFR-1 performance limits are requirements. Keep startup lazy, caches
  bounded and rendering free from diagnostic logging.
- GTK's main loop is the only event loop. Use `glib::spawn_future_local`.
  Never add Tokio or async-std.
- Every user command must go through the typed action layer in `window`
  (NFR-6.2). Direct view manipulation may stay in `viewer`; keys, menus and
  command-equivalent controls must not create separate behavior paths.
- `folder` must remain plain Rust without GTK or GIO types so another UI can
  reuse it (NFR-6.1).

## Module ownership

Put a rule in the module that owns it. Do not copy the same rule into another
module.

| Module | Owns | Does not own |
| --- | --- | --- |
| `main` | GTK application lifecycle, activation and single-instance entry | custom IPC or media behavior |
| `config` | defaults, parsing, extensions and key bindings | GTK UI or runtime media state |
| `folder` | sorted paths, insertion, removal and navigation | GTK, GIO, monitoring or decoding |
| `loader` | asynchronous glycin decoding and the bounded image cache | UI assembly or file writes |
| `annotation` | bounded shapes and shared preview/copy drawing | window actions, clipboard ownership or file writes |
| `viewer` | zoom, pan, fit, view rotation and source/view transforms over any `GdkPaintable` | file policy or GStreamer |
| `player` | GStreamer setup, playback, seeking and stream selection | window assembly or image decoding |
| `fileops` | trash, restore and rotate-save | every other persistent write |
| `window` | UI assembly, folder monitoring, media state, overlays and actions | codec code or copied business rules |
| `log` | timed diagnostic messages | render-path logging |

## Important implementation details

### Rust and GTK

- Do not take two `RefCell` borrows in one statement. A borrow panic inside a
  GTK `extern "C"` callback aborts the process. Read each needed value before
  calling back into a widget. See the scroll code in `viewer.rs`.
- Every asynchronous media result must compare `App::generation` before it
  changes the UI. Late decode, animation, metadata and SVG results must drop
  themselves instead of showing old media.
- Never log from `ImageView::snapshot` or another render path.
- Quick Markup shapes use decoded-image coordinates. Preview and clipboard
  copy must share drawing code and work through all rotations, pan, zoom and
  fractional scales.
- `gtk::Application` already provides D-Bus single-instance behavior. Use its
  `open` signal. Do not add a lock file, socket or custom IPC.
- The `gtk4` crate exposes GTK 4.0 by default. Enable the matching `v4_x`
  feature before using a newer GTK API. The target currently uses GTK 4.22.
- A glycin crate version must match the protocol supported by the installed
  glycin loaders. A mismatch can compile and still fail at runtime.

### Wayland and rendering

- Do not add X11 assumptions or client-side window movement. Use the
  compositor's `begin_move` and `begin_resize`. The frameless window owns its
  resize border (FR-6.4).
- Account for the surface scale. At 125% and 150%, 100% zoom must still map one
  image pixel to one physical pixel (FR-4.7).
- `update_cursor` is the only cursor owner. Priority is resize cursor, Quick
  Markup crosshair, hidden cursor, then the default cursor. Mark chrome hidden
  before calling `update_cursor`.

### Images and file operations

- glycin decodes outside the app in a sandbox. Loader failure is normal. An
  empty `supported_mime_types()` result usually means loader programs are not
  installed.
- `config::IMAGE_EXTENSIONS` is static on purpose. A startup D-Bus lookup would
  slow the first frame and make `folder` depend on GIO. When adding a format,
  update `config`, `install.sh` and the desktop entry together. Tests keep the
  list aligned with installed loaders and desktop MIME registration.
- JPEG rotate-save changes orientation without re-encoding pixels (FR-5.4).
  SVG and animated images are view-rotate only.
- Do not list `trash://` through gvfs. It can hang without a running GUI main
  loop. Restore reads freedesktop trash folders directly.
- Undo must handle the same file being added once by restore and once by the
  folder monitor.
- `preload_neighbors` may start a rare duplicate decode. Measurements showed
  that this is cheaper and safer than moving its apply guard away from
  `App::generation`. Change it only with new evidence.

### Video and subtitles

- Seek with `FLUSH | ACCURATE`, never `KEY_UNIT`. One-GOP clips otherwise jump
  to 0:00. `SeekState` allows one seek at a time and keeps only the latest
  requested position.
- Start GStreamer only when the first video opens. Never put videos in the
  decoded image cache. Set a reused pipeline to `Null` while showing an image.
- External `suburi` playback is the pipeline-reuse exception. Create a fresh
  `playbin3` when entering or leaving it because GStreamer 1.28 can keep stale
  subtitle-pad ownership.
- libav is a fallback. After lazy `gst::init`, raise installed Intel QSV
  decoders to `Primary + 1` unless their rank is explicitly `None`. An
  oversized, incorrectly levelled H.264 stream may bypass only `qsvh264dec`.
  Restore its rank on setup, error, navigation and close.
- Release idle inhibit on pause, image switch, playback error and close.
- The seek bar's `change-value` callback returns `Propagation::Proceed` so GTK
  moves the thumb. Position polling must not fight a scrub. Track scrubbing
  from raw button events because GTK may cancel a gesture.
- A stream collection may change during playback. Any selection change must
  preserve the chosen video, audio and subtitle streams together.

### Input

- Scroll values include a unit. `Wheel` means detents; `Surface` means logical
  pixels. Both occur on the target touchpad. Route them through the testable
  `viewer::ScrollGesture` logic.
- Space and arrow keys depend on context. Space controls video and advances a
  still image. Arrows pan zoomed media; otherwise they navigate or change
  volume. Page Up and Page Down always navigate the folder.
- Quick Markup owns primary drag before pan and window movement, but the resize
  border still wins.

## Verification

Run these checks for a complete code or packaging change:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
shellcheck install.sh uninstall.sh packaging/build-rpm.sh
sh -n install.sh uninstall.sh packaging/build-rpm.sh
desktop-file-validate data/io.github.TheRealShek.OpenMpv.desktop
appstreamcli validate --no-net data/io.github.TheRealShek.OpenMpv.metainfo.xml
rpmspec -P packaging/fedora/open-mpv.spec >/dev/null
```

`cargo test` performs real trash and rotate-save operations. It needs a user
session and ImageMagick.

For asynchronous media changes, test stale-result cancellation and bounded
resource use. Start with narrow tests, then run the full list. Report each
check as passed, failed, skipped or inspected.

### Human Wayland testing

Keyboard, pointer and screenshot automation is not reliable on the target
GNOME/Wayland session. Never claim an automated visual check. State exactly
what a person still needs to inspect.

Actions can be called without window focus:

```sh
gdbus call --session --dest io.github.TheRealShek.OpenMpv \
  --object-path /io/github/TheRealShek/OpenMpv/window/1 \
  --method org.gtk.Actions.Activate <action> "[]" "{}"
```

Prefer unit tests for decisions such as `nav_target`, `skip_target`,
`cursor_name` and `help_line` instead of reading the widget tree.

### Performance and resources

- Measure memory with PSS, not RSS. GTK and Mesa share many pages.
- Recheck cold start after changing startup or lazy initialization.
- Recheck hardware decoding after changing pipelines, filters or streams.
- After repeated navigation, make sure cache bytes, file descriptors, threads,
  loader processes and pipelines settle instead of growing forever.

## Git and CI

`main` is protected. Never push directly to it.

For a requested GitHub change:

1. Start a branch from the latest `main`.
2. Keep commits clear and limited to one purpose. Never add co-author trailers.
3. Push and open a pull request only when the user asks.
4. Wait for the required GitHub check. Every check required for the change
   must pass; only the intentional docs-only skip is allowed.
5. Complete any required human Wayland testing.
6. Merge only when the user asks and the author validation gate below is met.

The full Fedora job runs for any code, script, workflow, package, application
metadata or build configuration change. When a change contains only Markdown,
files under `docs/`, or `LICENSE`, a small classifier runs and the expensive
Fedora job is intentionally skipped. GitHub reports a conditionally skipped
job as successful, so the protected branch is not left waiting for a check.
If classification fails, the full Fedora job runs instead of failing open.

Do not use commit-message CI skip instructions or workflow-level path filters.
They leave a required workflow in Pending state and block the pull request.

### Author validation gate

Before merging a major change, the author must do one of these for that exact
change:

- personally test the completed behavior and confirm it; or
- clearly approve merging without personal testing.

Use this gate for major features and for changes with meaningful UI, playback,
file-operation, data-safety, compatibility or performance impact. Branch work
and CI may finish first. Record what passed, what still needs human testing and
the remaining risk before merge. Approval for one change does not cover the
next change.
