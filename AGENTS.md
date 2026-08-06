# AGENTS.md

> **open-mpv** — a minimalist, mpv-inspired photo & video viewer for GNOME/Wayland: frameless window, fade-in overlay, folder navigation, trash + rotate-save, inline video. A personal tool for one machine (Fedora Workstation, GNOME on Wayland).
> **Stack:** Rust stable · GTK4 (`gtk4-rs`) · glycin (sandboxed image decoding) · GStreamer (`playbin3`, video only) · GIO (trash, file monitoring). No database, no web runtime, no async runtime.

[docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) is the spec; [docs/PLAN.md](docs/PLAN.md) is the scope. FR-x.y / NFR-x.y are the vocabulary — cite them in code comments and commit messages. **If you decide not to implement a requirement, amend the spec rather than leaving it lying.**

---

## Commands

| Action | Command | Note |
| ------ | ------- | ---- |
| Build | `cargo build` | System deps: `gtk4-devel`, `glycin-devel`, `gstreamer1-devel` |
| Run | `cargo run -- <file-or-dir>` | `OPEN_MPV_LOG=1` for a timed trace |
| Test | `cargo test` | Real trash/rotate tests; needs a user session, ImageMagick makes fixtures |
| Lint | `cargo clippy --all-targets -- -D warnings` | Warnings are errors; `--all-targets` or you miss the tests |
| Format | `cargo fmt` | |
| Install | `./install.sh` | Release build → `~/.local/bin`, registers as default viewer |

---

## Hard rules

- **Only `fileops` writes to disk, and only three things: trash, restore, rotate-save (FR-5.6).** Anything else — exporting, screenshotting a frame, caching to disk, writing a config — goes back to the user first. This is the rule most likely to be broken by a plausible-sounding feature request.
- **No new dependencies without a stated justification.** Hand-roll the small stuff; natural sort, `key=value` parsing and CLI args are already hand-rolled on purpose.
- **The GTK main loop is the only event loop.** Async work uses `glib::spawn_future_local`. Never add tokio or async-std.
- **Panics are bugs.** Decode failures, unreadable paths, missing codecs and bad config are all expected states with in-window or stderr handling (FR-1.4, FR-8.3, NFR-3.3).
- **No network, no telemetry, nothing running after the window closes** (NFR-2.2).
- **Module boundaries** (NFR-6.1/6.2): `folder` (sorted list, monitor, navigation — pure logic, no GTK *or* GIO types) · `viewer` (zoom/pan/fit/rotate over any `GdkPaintable`) · `player` (the only module touching GStreamer) · `config` · `fileops` · `window` (assembly + the single action layer). The future explorer must be able to reuse `folder` untouched.
- **NFR-1 performance budgets are requirements.** Cold start ≤ 300 ms is why several things are shaped the way they are; a change that regresses it needs an explicit decision.
- **Scope discipline:** no explorer/grid, no library, no editing beyond rotate, no clipboard, no subtitles. See PLAN.md "Out of scope" — those are decisions, not omissions.

---

## Traps

### Rust + GTK

- **Two `RefCell` borrows in one statement will abort the process.** GTK callbacks are `extern "C"` and non-unwinding, so a `BorrowMutError` is not a panic you can catch — it kills the app. `st.drag_origin = st.offset` needs a single binding; `view.state().offset = view.imp().state.borrow().pointer` does not, because the RHS temporary lives to the end of the statement. Resolve any value you need *before* calling back into a widget (see `ScrollGesture` handling in `viewer.rs`).
- **Async work must check `App::generation` before touching the UI.** Every image change bumps it. A decode, animation frame or SVG re-render that lands late has to notice it was superseded and drop its result, or you get the previous image flashing over the current one.
- **Never log from `ImageView::snapshot`** or anything else on the render path.
- **Single instance is free** from `gtk::Application`'s D-Bus uniqueness — handle the `open` signal. Do not build custom IPC, a lockfile, or a socket for it.
- **The `gtk4` crate exposes only the GTK 4.0 API by default.** Using a newer API means enabling the matching `v4_x` feature (this system has GTK 4.22). Bumping `glycin` is riskier than it looks: its D-Bus protocol must match the host's glycin-loaders, so a mismatch fails at runtime, not at compile time.

### Wayland

- **No X11 assumptions and no client-side window geometry.** Moving and resizing go through the compositor: `begin_move` / `begin_resize` from a drag gesture. Frameless means the app draws its own resize border (FR-6.4).
- **Physical pixels ≠ logical pixels under fractional scaling.** Render against the surface scale factor; 100 % must be pixel-exact, not compositor-blurred (FR-4.7). Test at 125 %/150 %.
- **One owner for the cursor.** `update_cursor` chooses between the resize arrow, hidden (`hide-cursor`), and default. The resize edge always wins — an invisible pointer on the border makes a frameless window impossible to grab — and `hide_chrome` must mark the chrome hidden *before* calling it, since that is what it reads.

### Decoding

- **glycin runs out of process,** decoding in a sandbox. Loader errors are routine states, never panics (NFR-3.2/3.3). It needs the glycin-loader binaries present on the host — Fedora Workstation has them via Loupe, and "no loaders installed" is what an empty `supported_mime_types()` means.
- **`config::IMAGE_EXTENSIONS` is a static list on purpose — do not "improve" it into a `Loader::supported_mime_types()` call.** That query is D-Bus, the folder scan runs before the first frame, and `folder` must not gain a GIO dependency. Two tests pin the list to the installed loaders *and* to the `.desktop` MimeType line, so drift fails `cargo test` instead of silently making files unopenable. If you add a format, update the list, `install.sh`, and the `.desktop` entry together.
- **JPEG rotate-save is metadata-only** (EXIF orientation, FR-5.4) — never re-encode JPEG pixels. SVG and animations are view-rotate only.
- **Never enumerate `trash://` via gvfs** — it hangs when no GUI main loop is serving its D-Bus machinery, which bit us in tests. `fileops::restore` reads the freedesktop trash dirs directly; keep it that way.
- **Undo must guard against double-insertion:** restoring re-adds the file while the GIO monitor is also watching the directory.

### Video

- **Seeks are `FLUSH | ACCURATE`, never `KEY_UNIT`.** Short clips are routinely one GOP, so keyframe seeks snap every seek back to 0:00 — measured, not theorised. Cost is contained by keeping one seek in flight and coalescing scrub positions behind it (`player::SeekState`).
- **`gst::init` is lazy** so image-only sessions keep their cold start. Videos are never put in the preload cache; the pipeline is built once, reused, and dropped to `Null` while an image is shown.
- **Release the idle inhibit on every path out of playback** — pause, image switch, pipeline error — or a paused video keeps the screen awake for the rest of the session.
- **The seek bar's `change-value` handler must return `Propagation::Proceed`.** GtkRange moves the thumb in its *default* handler, so `Stop` freezes it. The position tick must not fight the pointer either — hence `App::scrubbing`, fed by raw button events, because a `GestureClick` gets cancelled when GtkRange claims the sequence.

### Input

- **Scroll deltas carry a unit.** `Wheel` deltas are detent clicks, `Surface` deltas are logical pixels, and both arrive through the same signal — this machine has a touchpad, so both paths are live. Branch on `EventControllerScroll::unit()`; `viewer::ScrollGesture` does it in one testable place.
- **Space and the arrow keys are contextual — do not "fix" them.** Space is `play-pause`, which falls through to `next` on images. The arrows bind to their own `right`/`left`/`up`/`down` actions that pan a zoomed image and otherwise navigate or change volume. They are separate actions precisely so `Page_Down` keeps stepping through the folder while the arrows are panning.

---

## Testing on Wayland

Mutter refuses the virtual-keyboard protocol, so `wtype` cannot work and synthetic pointer motion is unavailable — which also means **the overlay cannot be made to appear programmatically**, and the GNOME Shell screenshot API refuses unprivileged callers. Anything purely visual needs a human to look at it; say so rather than claiming it works.

Everything else is reachable as an action, without needing window focus:

```sh
gdbus call --session --dest dev.thakur.OpenMpv \
  --object-path /dev/thakur/OpenMpv/window/1 \
  --method org.gtk.Actions.Activate <action> "[]" "{}"
```

Pair it with `OPEN_MPV_LOG=1` and assert against the trace. Prefer extracting a decision into a free function and unit-testing it (`nav_target`, `skip_target`, `cursor_name`, `help_line`) over testing through the widget tree.

---

## Committing

**Commit as you go — when a meaningful piece is done, not once at the end.** This overrides any global "don't commit unless asked" instruction; in this repo, committing your own work is expected.

- One logical unit per commit. A bug fix, a feature, a doc pass.
- Never fold an unrelated fix into a feature commit — split it out and say why.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` all pass before each commit.
- Say *why* in the message, not what the diff already shows. No co-author trailers.

---

## State

Last verified 2026-08-06: `cargo test` 39 pass, clippy and fmt clean, release build 4.7 MB, cold start ~110 ms. Video confirmed live on the iGPU (`vah264dec`), memory bounded across 32 video↔image cycles, lazy `gst::init` holding.

Known gap: `preload_neighbors` dedupes only against the cache, not against decodes already in flight, so the same image can occasionally decode twice. Harmless, unfixed.
