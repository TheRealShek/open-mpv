# AGENTS.md

> **Project:** **open-mpv** — a minimalist, mpv-inspired photo viewer for GNOME/Wayland (frameless window, overlay controls, folder navigation, trash + rotate-save). Personal tool, built for one machine: Fedora Workstation, GNOME on Wayland.
> **Stack:** Rust (stable) · GTK4 via `gtk4-rs` · glycin (sandboxed image decoding) · GIO for trash/file-watching · no database, no web runtime.

Docs: [docs/PLAN.md](docs/PLAN.md) (vision, scope, technical approach) and [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) (FR/NFR — the spec). Read both before changing behavior; requirement IDs (FR-x.y / NFR-x.y) are the vocabulary for discussing features.

---

## Commands

| Action | Command | Note |
| ------ | ------- | ---- |
| Build | `cargo build` | System deps: `gtk4-devel`, `glycin-devel` (Fedora) |
| Run | `cargo run -- <image-or-dir>` | No arg opens the empty-state window |
| Test | `cargo test` | Unit + integration; UI-free logic must be testable headless |
| Lint | `cargo clippy -- -D warnings` | Warnings are errors |
| Format | `cargo fmt` | Run before finishing any change |

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
- Keep the dependency tree minimal. Core: `gtk4`, `glycin`, `glib`/`gio` (ship with gtk4-rs). Hand-roll small things (natural sort, `key=value` config parsing, CLI args via `std::env`) instead of adding crates; a new dependency needs a stated justification.
- Module boundaries (NFR-6.1/6.2): `folder` (sorted image list, GIO file monitor, navigation — no GTK types) · `viewer` (display widget: zoom/pan/fit/rotate) · `actions` (single action layer; keybindings and overlay buttons both dispatch through it) · `config` (parse `~/.config/open-mpv/open-mpv.conf`) · `fileops` (trash/undo, atomic rotate-save). The future explorer reuses `folder` untouched.
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

---

## Rules

- The only permitted file writes are trash, trash-restore, and rotate-save (FR-5.6). A task that seems to need any other write goes back to the user first.
- No network access, no telemetry, no background process after the window closes (NFR-2.2).
- Performance budgets in NFR-1 are requirements, not aspirations — a change that regresses cold-start or navigation latency needs an explicit trade-off decision.
- Scope discipline: no explorer/grid, no library, no editing beyond rotate in this iteration (PLAN.md "Out of scope").

---

## Verified

Last verified: not yet verified — commands assume the `cargo` scaffold, which does not exist yet; verify and update this line once the project builds.
