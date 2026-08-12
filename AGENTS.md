# AGENTS.md

## Start here

**open-mpv** is a minimalist, mpv-inspired photo and video viewer for one
machine: Fedora Workstation, GNOME and Wayland. The window is frameless, the
controls fade away, and local images and videos share one folder-navigation
flow.

**Stack:** Rust stable · GTK4 (`gtk4-rs`) · glycin (sandboxed image decoding) ·
GStreamer (`playbin3`, video only) · GIO (trash and file monitoring). There is
no database, web runtime or async runtime.

The author is **Abhishek**. Address him as **Sir**.

Read these sources before changing behavior:

1. [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) is the product specification.
2. [docs/PLAN.md](docs/PLAN.md) defines current scope and deliberate exclusions.
3. A GitHub issue, when one exists, defines the requested outcome and open
   decisions for that piece of work.
4. This file defines engineering constraints and known traps.

Use FR-x.y and NFR-x.y as the project vocabulary in relevant code comments and
commit messages. If an approved change alters a requirement or moves something
into scope, update the requirements and plan in the same logical change. Never
leave the documents knowingly contradicting the product.

---

## Product intent and boundaries

The product should feel **instant, invisible and predictable**:

- Open a local file or folder and show media quickly.
- Keep the media edge-to-edge; reveal only contextual overlay controls.
- Make mouse, touchpad and keyboard equally usable.
- Keep optional configuration in one small mpv-style text file.
- Never surprise the user with file changes.

The current product includes folder navigation, broad glycin image support,
animations, sharp SVG rendering, inline local video, embedded and local
SRT/WebVTT subtitles, zoom/pan/fit/rotate, lossless JPEG rotation, trash with
undo, fullscreen, configurable actions and GNOME desktop integration.

The product is a viewer, not a manager or editor. Unless the author explicitly
changes the scope, do not add:

- a media library, indexing, tagging, ratings, search or cloud features;
- editing beyond rotate-save;
- network access, telemetry, background services or an in-app updater;
- clipboard/export/screenshot writes;
- subtitle downloading, editing, timing/style controls or dual subtitles;
- general media-player growth such as playlists or online playback;
- support for non-Linux platforms.

Do not treat a plausible feature as permission to expand scope. Explain the
trade-off and ask first when it would change the product boundary.

---

## Architecture and ownership

Keep responsibility in these modules:

| Module | Owns | Must not own |
| ------ | ---- | ------------ |
| `main` | GTK application lifecycle, activation and single-instance entry | custom IPC or media behavior |
| `config` | parsing, defaults, supported extensions and configured bindings | GTK UI or runtime media state |
| `folder` | pure sorted-path model, insertion/removal and navigation | GTK or GIO types, monitors or decoding |
| `loader` | async glycin decode and bounded decoded-image cache | UI assembly or file writes |
| `viewer` | zoom, pan, fit and view rotation over any `GdkPaintable` | file-type policy or GStreamer |
| `player` | all GStreamer setup, playback, seeking and stream selection | GTK window assembly or image decoding |
| `fileops` | trash, restore and rotate-save | any other persistent write |
| `window` | assembly, GIO folder monitor, media state, overlays and the single action layer | codec implementation or duplicated business rules |
| `log` | timed diagnostic trace | render-path logging |

Important architectural rules:

- `folder` must remain reusable by another UI surface without GTK/GIO changes
  (NFR-6.1).
- Every user command goes through the typed action layer in `window`
  (NFR-6.2). UI controls, defaults and configured keys must not create separate
  behavior paths.
- The GTK main loop is the only event loop. Use `glib::spawn_future_local` for
  async work. Never add Tokio or async-std.
- Reuse an existing helper or pattern when it fits. Search callers before
  changing shared behavior.

---

## Non-negotiable engineering rules

- **Only `fileops` writes to disk, and only for trash, restore and rotate-save
  (FR-5.6).** Exporting, screenshots, disk caches, generated thumbnails,
  history, state files or writing configuration require a product decision
  first.
- **No new dependency without a stated justification.** Prefer the standard
  library and existing GTK/GIO/Glycin/GStreamer capabilities. Natural sort,
  `key=value` parsing and CLI handling are deliberately hand-rolled.
- **Write Rust as Rust.** Model valid states with enums and types, keep
  ownership explicit, and use `Option`/`Result` instead of sentinels or
  out-parameters.
- **Panics are bugs.** Broken media, unreadable paths, missing codecs, invalid
  configuration and failed file operations are normal error states. Report
  them in-window or on stderr as specified (FR-1.4, FR-8.3, NFR-3.3).
- **No network, telemetry or lingering process** after the window closes
  (NFR-2.2).
- **Performance budgets are requirements.** Cold start must stay at or below
  300 ms and common neighbor navigation at or below 100 ms. Do not move lazy
  work into image startup or add unbounded caches without an explicit decision.
- Keep changes scoped. Do not combine unrelated cleanup, refactoring,
  formatting or upgrades with requested work.

---

## Subsystem invariants and traps

### Rust and GTK

- **Two `RefCell` borrows in one statement can abort the process.** GTK
  callbacks are `extern "C"` and non-unwinding, so a `BorrowMutError` cannot be
  safely caught. Resolve every needed value before calling back into a widget.
  See scroll-gesture handling in `viewer.rs` for the safe pattern.
- **Every async media result must check `App::generation` before touching the
  UI.** A late decode, animation frame, metadata result or SVG re-render must
  drop itself instead of flashing old media over the current item.
- Never log from `ImageView::snapshot` or another render path.
- `gtk::Application` already provides D-Bus uniqueness. Handle its `open`
  signal; do not add a lockfile, socket or custom IPC.
- The `gtk4` crate exposes GTK 4.0 by default. A newer API needs the matching
  `v4_x` feature. The target currently has GTK 4.22.
- A glycin crate bump must remain protocol-compatible with the host's
  glycin-loaders. A mismatch fails at runtime even when compilation succeeds.

### Wayland and rendering

- No X11 assumptions and no client-side window movement/resizing. Use the
  compositor's `begin_move` / `begin_resize`; the frameless window owns its
  resize border (FR-6.4).
- Physical and logical pixels differ under fractional scaling. Render using
  the surface scale so 100% remains pixel-exact at 125% and 150% scaling
  (FR-4.7).
- `update_cursor` is the single cursor owner. Resize cursor wins over hidden
  chrome, then `hide-cursor`, then default. `hide_chrome` must mark chrome
  hidden before asking `update_cursor`.

### Image decoding and editing

- glycin decodes out of process in a sandbox. Loader failure is routine, not a
  panic (NFR-3.2/3.3). An empty `supported_mime_types()` generally means no
  loader binaries are installed.
- `config::IMAGE_EXTENSIONS` is static on purpose. Do not replace it with a
  startup D-Bus query: the folder scan runs before the first frame and
  `folder` must stay independent of GIO. Tests keep this list aligned with
  installed loaders and the desktop MIME list. When adding a format, update
  `config`, `install.sh` and the desktop entry together.
- JPEG rotate-save changes orientation metadata without re-encoding pixels
  (FR-5.4). SVG and animated images remain view-rotate only.
- Do not enumerate `trash://` through gvfs. It can hang without a serving GUI
  main loop. Restore reads freedesktop trash directories directly.
- Undo must tolerate the same file being reintroduced by restore and by the
  directory monitor.

### Video and subtitles

- Seeks use `FLUSH | ACCURATE`, never `KEY_UNIT`. Short one-GOP clips otherwise
  snap every seek to 0:00. `SeekState` permits one seek in flight and coalesces
  later scrub positions.
- GStreamer initialization stays lazy so image-only startup loads no plugins.
  Videos never enter the image preload cache. Reused pipelines drop to `Null`
  while an image is shown.
- An external `suburi` session is the deliberate pipeline-reuse exception.
  Entering or leaving one gets a fresh `playbin3`; GStreamer 1.28 can retain
  stale text-pad ownership across `Null` and race text ahead of video.
- libav is a software fallback, not the default path. After lazy `gst::init`,
  installed QSV decoders are raised to `Primary + 1`, except an explicit
  `Rank::None`. Oversized, incorrectly levelled H.264 temporarily bypasses only
  `qsvh264dec`; restore its rank symmetrically on decoder setup, error,
  navigation and close.
- Release idle inhibit on every route out of active playback: pause, image
  switch, error and close.
- The seek bar's `change-value` handler returns `Propagation::Proceed`; GTK's
  default handler moves the thumb. Position polling must not fight scrubbing.
  `App::scrubbing` therefore follows raw button events rather than a gesture
  GTK may cancel.
- Stream collections can change during one video. Any future selection logic
  must preserve the chosen video, audio and subtitle streams together rather
  than sending a partial selection event.

### Input behavior

- Scroll deltas include a unit. `Wheel` values are detents; `Surface` values
  are logical pixels. Both are active on the target touchpad. Branch through
  the testable `viewer::ScrollGesture` behavior.
- Space and arrow keys are intentionally contextual. Space controls video and
  advances still images. Arrows pan zoomed media; otherwise they navigate or
  adjust volume. Page Up/Down must keep folder navigation independent of that
  context.

---

## Commands

| Action | Command | Note |
| ------ | ------- | ---- |
| Build | `cargo build` | Needs `gtk4-devel`, `glycin-devel`, `gstreamer1-devel` |
| Run | `cargo run -- <file-or-dir>` | Timed trace is on; `OPEN_MPV_LOG=0` disables routine logging |
| Test | `cargo test` | Includes real trash/rotate tests; needs a user session; ImageMagick creates fixtures |
| Lint | `cargo clippy --all-targets -- -D warnings` | `--all-targets` is required to cover tests |
| Format | `cargo fmt --check` | Use `cargo fmt` to apply formatting |
| Install | `./install.sh` | Release build to `~/.local/bin`; registers default MIME handlers |

---

## Verification

Start with the narrowest relevant test, then run the complete required checks:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Test the expected path, important failures and relevant edge cases. For bugs,
add regression coverage when practical. For concurrency or async changes,
verify cancellation/stale-result guards and bounded resource use. Report only
checks actually run; distinguish automated checks, code inspection and human
visual testing.

### Testing on Wayland

Mutter rejects the virtual-keyboard protocol, so `wtype` cannot drive the app
and synthetic pointer motion is unavailable. The overlay therefore cannot be
revealed programmatically, and GNOME Shell's screenshot API refuses
unprivileged callers. **Anything purely visual needs a human to inspect it.**
Never claim automated visual verification.

Actions can be invoked without window focus:

```sh
gdbus call --session --dest io.github.TheRealShek.OpenMpv \
  --object-path /io/github/TheRealShek/OpenMpv/window/1 \
  --method org.gtk.Actions.Activate <action> "[]" "{}"
```

Assert against the default trace where useful. Prefer extracting decisions
into free functions and unit-testing them (`nav_target`, `skip_target`,
`cursor_name`, `help_line`) over probing the widget tree.

### Performance and resource checks

- Measure memory with **PSS, not RSS**. Shared GTK/Mesa pages make RSS
  misleading.
- Recheck cold start when startup or lazy initialization changes.
- Recheck hardware decoding when the player pipeline, filters or stream
  selection changes.
- For caches, monitors, loaders and pipelines, verify that bytes, file
  descriptors, threads and child processes settle after repeated navigation.

---

## Git workflow

**Commit as meaningful pieces are completed, not once at the end.** This
repository-specific rule overrides the general default that commits need a
separate request. It does not grant permission to push.

- Keep one logical unit per commit: a fix, a feature or a documentation pass.
- Never fold an unrelated fix into another change.
- Before every commit, ensure `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings` and `cargo test` pass.
- Explain why in the commit message. Never add co-author trailers.
- Never create a branch, rebase, reset or push unless the author explicitly
  asks for that operation.

### Author gate for major issues

**Before work for any major issue is pushed, Abhishek must either personally
test the completed behavior and confirm it, or explicitly approve pushing it
without his personal test.**

- Apply this gate to substantial features and to changes with meaningful UI,
  playback, file-operation, data-safety, compatibility or performance impact.
- The gate is per issue/change. Approval for one does not carry to another.
- Implementation, local verification and commits may be completed first. Then
  report what passed, what still needs human testing and any remaining risk.
- A request to implement or commit is not permission to push.
- Minor documentation or maintenance work does not need personal testing, but
  pushing it still requires explicit permission.

---

## Current measured state

Last performance and resource audit: **2026-08-06**. The release binary was
4.7 MB, cold start was about 110 ms, and scanning 5001 entries took 3.7 ms.
The automated suite had grown to 69 passing tests by **2026-08-12**, with
clippy and formatting clean.

Measured on the target machine:

- empty window: 163 MB RSS / 54 MB PSS;
- 12 MP photo: 203 MB RSS / 93 MB PSS;
- video: 251 MB RSS / 129 MB PSS;
- paused video: 0.1% CPU;
- image-only session: zero GStreamer plugins loaded.

No sustained leak was found across 200 navigations, 30 video/image cycles or a
playback soak: memory settled and descriptors, threads and loader processes
returned. Compatible video decoded on the Intel iGPU through VA-API while EGL
rendered through Mesa.

Fedora's optional `gstreamer1-plugin-libav` package provides the software
fallback. The motivating failure was H.264 High at 4382×3500 while declaring
Level 4: QSV advertises a 4096-pixel limit, OpenH264 rejected every frame and
FFmpeg decoded it. Do not replace the measured hardware-first path with blanket
software decoding.

Known accepted gap: `preload_neighbors` deduplicates against cached images but
not decodes already in flight. It measured 202 decodes for 200 cache hits over
200 steady-state navigations. Fixing it would require moving the apply guard
off the generation counter, and the risk is not justified by the measured
cost.
