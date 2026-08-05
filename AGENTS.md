# AGENTS.md

> **Project:** **open-mpv** — a minimalist, mpv-inspired photo & video viewer for GNOME/Wayland (frameless window, overlay controls, folder navigation, trash + rotate-save, inline video playback). Personal tool, built for one machine: Fedora Workstation, GNOME on Wayland.
> **Stack:** Rust (stable) · GTK4 via `gtk4-rs` · glycin (sandboxed image decoding) · GStreamer via `gstreamer-rs` (video, FR-10) · GIO for trash/file-watching · no database, no web runtime.

Docs: [docs/PLAN.md](docs/PLAN.md) (vision, scope, technical approach) and [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) (FR/NFR — the spec). Read both before changing behavior; requirement IDs (FR-x.y / NFR-x.y) are the vocabulary for discussing features.

---

## Commands

| Action | Command | Note |
| ------ | ------- | ---- |
| Build | `cargo build` | System deps: `gtk4-devel`, `glycin-devel` (Fedora) |
| Run | `cargo run -- <image-or-dir>` | No arg opens the empty-state window |
| Test | `cargo test` | Includes real trash/rotate integration tests (need user session; ImageMagick generates fixtures) |
| Lint | `cargo clippy -- -D warnings` | Warnings are errors |
| Format | `cargo fmt` | Run before finishing any change |
| Install | `./install.sh` | Release build → `~/.local/bin`, registers default viewer |

---

## Working Agreement

- Read nearby code and tests before changing behavior; follow established project conventions.
- Prefer the smallest cohesive change that fully solves the request.
- Preserve public behavior unless the task explicitly requires a breaking change.
- Add or update tests for changed behavior, including meaningful failure cases.
- Use existing abstractions and dependencies before introducing new ones.
- Return errors with enough context to diagnose the failed operation without exposing secrets.
- Run the relevant formatter, static checks, and tests before finishing.
- Update documentation when commands, configuration, or public behavior changes.

## Language Guidelines

- Rust stable only; no nightly features.
- The GTK main loop is the only event loop — run async work (glycin decoding) with `glib::spawn_future_local`; do **not** add tokio/async-std.
- Keep the dependency tree minimal. Core: `gtk4`, `glycin`, `glib`/`gio` (ship with gtk4-rs), `gstreamer` (video playback, FR-10 — bindings only; the C libs are system-stock). Hand-roll small things (natural sort, `key=value` config parsing, CLI args via `std::env`) instead of adding crates; a new dependency needs a stated justification.
- Module boundaries (NFR-6.1/6.2): `folder` (sorted image list, GIO file monitor, navigation — no GTK types) · `viewer` (display widget: zoom/pan/fit/rotate over any `GdkPaintable`) · `player` (playbin3 → gtk4paintablesink; the only module touching GStreamer) · `actions` (single action layer; keybindings and overlay buttons both dispatch through it) · `config` (parse `~/.config/open-mpv/open-mpv.conf`) · `fileops` (trash/undo, atomic rotate-save). The future explorer reuses `folder` untouched.
- All file writes go through `fileops` and are atomic (temp + rename). No other module writes to the filesystem (FR-5.6).
- Panics are bugs: decode failures, unreadable paths, and bad config are expected states with in-window/stderr handling (FR-1.4, FR-8.3, NFR-3.3).

---

## Gotchas

- **gtk4-rs feature flags:** the `gtk4` crate exposes only the GTK 4.0 API by default — enable the `v4_x` feature matching the APIs used (system has GTK 4.22). Likewise pin a glycin crate version whose D-Bus protocol matches the system's glycin-loaders 2.1.5; verify at scaffold time.
- **Wayland-first:** no XWayland assumptions. Frameless = no titlebar; window move must use the compositor drag protocol (`begin_move` from the drag gesture) — there is no client-side window repositioning on Wayland.
- **Physical pixels (FR-4.7):** with fractional scaling, logical size ≠ pixel size. Render against the surface scale factor and test at 125 %/150 % scaling before calling scaling work done.
- **glycin is an out-of-process sandboxed loader:** loader errors are routine states (NFR-3.2/3.3), never panics. It needs glycin loader binaries on the host (present on Fedora Workstation via Loupe).
- **JPEG rotate-save is metadata-only** (EXIF orientation, FR-5.4) — never re-encode JPEG pixels. SVG/animated: view-rotate only, save disabled.
- **Single instance** comes free from `gtk::Application` D-Bus uniqueness — handle the `open` signal; don't build custom IPC.
- **Delete advances then toasts (FR-5.1/5.2):** undo must restore from trash *and* re-insert at the correct sorted position while the GIO file monitor is also watching the directory — guard against double-insertion.
- **Video decodes in-process** (FR-10.6): GStreamer has no glycin-style sandbox — an accepted trade-off; pipeline errors are routine in-window states. `gst::init` runs lazily on the first video so image-only sessions keep cold-start (NFR-1.1). Videos are never put in the preload cache; the `Player` pipeline is built once and reused, `Null` while an image is shown.
- **Space is contextual:** bound to `play-pause`, which toggles video playback and falls through to `next` on images — don't "fix" it to a plain `next` bind.
- **Seeks are accurate, never keyframe:** short clips are routinely encoded as a single GOP, so `GST_SEEK_FLAG_KEY_UNIT` snaps *every* seek back to 0:00 — measured on this machine's library, a 5 s clip landed at 0.00 s for targets of 1.26/2.52/3.78 s. `FLUSH|ACCURATE` lands exact for 2–455 ms. Cost is contained by keeping at most one seek in flight and coalescing scrub positions behind it (`player::SeekState`).
- **The seek bar's `change-value` handler must return `Propagation::Proceed`:** GtkRange moves the thumb to the pointer in its *default* handler, so returning `Stop` freezes the thumb and only the position tick can move it. The tick in turn must not write the thumb while the pointer holds it — hence `App::scrubbing`, fed by raw button events (a `GestureClick` gets cancelled when GtkRange claims the sequence).
- **Never enumerate `trash://` via gvfs:** it hangs when no GUI main loop is serving its D-Bus machinery (bit us in tests). `fileops::restore` reads the freedesktop trash dirs (home + mount-level `.Trash-$uid`) directly; keep it that way.

---

## Rules

- The only permitted file writes are trash, trash-restore, and rotate-save (FR-5.6). A task that seems to need any other write goes back to the user first.
- No network access, no telemetry, no background process after the window closes (NFR-2.2).
- Performance budgets in NFR-1 are requirements, not aspirations — a change that regresses cold-start or navigation latency needs an explicit trade-off decision.
- Scope discipline: no explorer/grid, no library, no editing beyond rotate in this iteration (PLAN.md "Out of scope").

---

## Verified

Last verified: 2026-08-05 — `cargo test` (21 pass, incl. trash round-trip, JPEG rotate-save, cache byte-budget eviction), `cargo clippy -- -D warnings`, `cargo fmt`, release build 4.6 MB.

Video (FR-10) verified live on this machine: `vah264dec` selected for H.264 (VA-API on the Intel iGPU), EOS loop restarts on schedule, transport actions driven over the exported `org.gtk.Actions` bus (pause/resume/seek/mute confirmed in the log), pipeline released on every video→image switch, corrupt MP4 reports in-window without crashing. Memory across 32 video↔image cycles stayed bounded (peak ~295 MB, settled 233 MB — allocator arena, not growth). Lazy `gst::init` confirmed: an image-only session loads zero GStreamer plugins (1.7 MB PSS from linked libs) and cold-starts in 187–202 ms, unchanged from before video support. Mixed-folder run covered animated GIF → WebM → JPEG → SVG including sharp SVG re-render at zoom.

`gdbus call --session --dest dev.thakur.OpenMpv --object-path /dev/thakur/OpenMpv/window/1 --method org.gtk.Actions.Activate <action> "[]" "{}"` drives any action without needing window focus — the way to test interactions on Wayland (Mutter refuses the virtual-keyboard protocol, so `wtype` cannot work).
