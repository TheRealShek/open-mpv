# AGENTS.md

## Start here

**open-mpv** is a minimalist local photo and video viewer for Fedora
Workstation, GNOME and Wayland. It uses Rust, GTK4, glycin, GStreamer and GIO.

Use each document for one purpose:

- [README.md](README.md): current user-facing capabilities, installation,
  controls and configuration.
- [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md): the authoritative, testable
  product behavior and performance budgets. Read it before changing behavior.
- [docs/PLAN.md](docs/PLAN.md): product intent, scope boundaries and deliberate
  exclusions. Read it before adding or expanding a capability.
- [docs/DISTRIBUTION.md](docs/DISTRIBUTION.md): packaging, release and update
  decisions. Read it for distribution work.
- This file: code ownership, engineering constraints, known implementation
  traps, verification and repository workflow.

Use FR-x.y and NFR-x.y as the project vocabulary in relevant code comments and
commit messages. If an approved change alters a requirement or moves something
into scope, update the requirements and plan in the same logical change. Never
leave the documents knowingly contradicting the product.

---

## Architecture and ownership

Keep responsibility in these modules:

| Module | Owns | Must not own |
| ------ | ---- | ------------ |
| `main` | GTK application lifecycle, activation and single-instance entry | custom IPC or media behavior |
| `config` | parsing, defaults, supported extensions and configured bindings | GTK UI or runtime media state |
| `folder` | pure sorted-path model, insertion/removal and navigation | GTK or GIO types, monitors or decoding |
| `loader` | async glycin decode and bounded decoded-image cache | UI assembly or file writes |
| `annotation` | bounded transient shape model and shared preview/copy drawing geometry | window actions, clipboard ownership or file writes |
| `viewer` | zoom, pan, fit, view rotation and source/view transforms over any `GdkPaintable` | file-type policy or GStreamer |
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

---

## Non-negotiable engineering rules

- **Only `fileops` writes to disk, and only for trash, restore and rotate-save
  (FR-5.6).** Exporting, screenshots, disk caches, generated thumbnails,
  history, state files or writing configuration require a product decision
  first. FR-11 clipboard publication is transient and never enters `fileops`.
- **No new dependency without a stated justification.** Prefer the standard
  library and existing GTK/GIO/Glycin/GStreamer capabilities. Natural sort,
  `key=value` parsing and CLI handling are deliberately hand-rolled.
- **Panics are bugs.** Broken media, unreadable paths, missing codecs, invalid
  configuration and failed file operations are normal error states. Report
  them in-window or on stderr as specified (FR-1.4, FR-8.3, NFR-3.3).
- **NFR-1 performance budgets are requirements.** Do not move lazy work into
  image startup or add unbounded caches without an explicit decision.

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
- Quick Markup geometry stays in decoded-image coordinates. Preview and copy
  must share one drawing implementation; source/view transforms must cover all
  four rotations, pan, zoom and fractional surface scales.
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
- `preload_neighbors` deliberately deduplicates cached images but not decodes
  already in flight. The rare duplicate decode measured cheaper and safer than
  moving its apply guard off `App::generation`; revisit only with new evidence.

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
- Quick Markup capture owns primary drag ahead of pan and window move, but the
  frameless resize-edge capture still wins. `update_cursor` remains the single
  cursor owner: resize first, then the markup crosshair over media, then normal
  chrome hiding.

---

## Verification

Run the complete required checks:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
shellcheck install.sh uninstall.sh
sh -n install.sh uninstall.sh
desktop-file-validate data/io.github.TheRealShek.OpenMpv.desktop
```

`cargo test` includes real trash and rotate-save tests, requires a user
session, and uses ImageMagick to create fixtures.

Async media changes must verify cancellation/stale-result guards and bounded
resource use. The report must distinguish automated checks from the human
Wayland checks described below.

### Testing on Wayland

The target GNOME/Wayland session cannot automate keyboard, pointer or
screenshots reliably. **Anything purely visual needs human inspection.** Never
claim automated visual verification.

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

## Git and CI

`main` is protected. Never push directly to it.

To merge a change:

1. Create a branch from the latest `main`.
2. Make clear, logical commits on that branch.
3. Push the branch and open a pull request to `main`.
4. Wait for the required GitHub CI check to pass.
5. Complete any needed human Wayland testing.
6. Merge the pull request. Never merge with failed or skipped required checks.

Local checks are useful but are not required before every commit. GitHub CI is
the required merge gate and runs the commands in the Verification section.

Agents may create local commits as meaningful pieces are completed. They must
not push, open a pull request or merge unless the user asks. Keep unrelated
changes separate, explain why in the commit message and never add co-author
trailers.

### Author validation gate

**Before a major change is merged, the author must either personally test the
completed behavior and confirm it, or explicitly approve merging it without
personal testing.**

- Apply this gate to substantial features and to changes with meaningful UI,
  playback, file-operation, data-safety, compatibility or performance impact.
- The gate is per issue/change. Approval for one does not carry to another.
- Branch work and CI may be completed first. Record what passed, what still
  needs human testing and any remaining risk before merging.
