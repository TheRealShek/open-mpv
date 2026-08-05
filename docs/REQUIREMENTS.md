# open-mpv — Requirements

Companion to [PLAN.md](PLAN.md). "Must" requirements define the finished
product; there is no version staging.

---

## 1. Functional Requirements

### FR-1 — Opening images

- **FR-1.1** The app opens an image from a file path given on the command
  line (`open-mpv <path>`), or from the desktop (double-click in Files,
  "Open with").
- **FR-1.2** Opening a file implicitly loads its folder context: all
  supported images in the same directory become the navigation set.
- **FR-1.3** Opening a directory path shows the first image of that
  directory (same sort order as FR-3.2).
- **FR-1.4** If the path is missing or unreadable, the app shows a clear
  in-window error state (not a crash, not a silent exit).
- **FR-1.5** Launching with no argument opens an empty window with a hint
  ("Open an image…") and accepts drag-and-drop of a file.

### FR-2 — Supported formats

- **FR-2.1** Still images: JPEG, PNG, WebP, AVIF, BMP.
- **FR-2.2** Animated images: GIF, animated WebP, animated PNG — play
  automatically, loop, with correct frame timing.
- **FR-2.3** Vector: SVG, rendered sharp at any zoom level (re-rasterized,
  not scaled bitmap).
- **FR-2.4** EXIF orientation is respected on load — photos always appear
  the right way up.
- **FR-2.5** Unsupported or corrupt files in a folder are skipped during
  navigation; opening one directly shows the FR-1.4 error state.

### FR-3 — Folder navigation

- **FR-3.1** Next/previous image via: arrow keys, on-screen overlay
  arrows, and horizontal scroll / Shift+scroll.
- **FR-3.2** Default order is natural filename sort (`img2` before
  `img10`), case-insensitive; config can switch to modification date
  (FR-8).
- **FR-3.3** Navigation does not wrap by default; at either end a subtle
  cue indicates "first/last image" (wrap available via config).
- **FR-3.4** A position indicator ("17 / 244") is visible in the overlay.
- **FR-3.5** If files are added, removed, or renamed in the folder while
  viewing, the navigation set updates without restarting the app.

### FR-4 — Viewing: zoom, pan, fit

- **FR-4.1** Images open in **fit-to-window** mode: scaled down to fit,
  never scaled up past 100 %.
- **FR-4.2** Zoom via scroll wheel (anchored at the cursor position),
  keyboard (`+`/`-`), and pinch gesture on a touchpad.
- **FR-4.3** One-key/one-click toggle between fit and 100 % (actual
  pixels); zooming at 100 %+ pans with mouse drag or arrow keys.
- **FR-4.4** Zoom range at least 5 %–2000 %; current zoom level shown
  briefly in the overlay when it changes.
- **FR-4.5** The view state (zoom/pan) resets to fit when navigating to
  another image.
- **FR-4.6** Window resize keeps the current fit mode (a fitted image
  re-fits; a zoomed image keeps its zoom).
- **FR-4.7 Scaling quality:** downscaling uses high-quality filtering
  (mipmapped/trilinear — no aliasing or shimmer while zooming or
  resizing); upscaling beyond 100 % uses smooth interpolation. On HiDPI
  and fractional scale factors, the image maps to **physical** pixels —
  a 100 % view is pixel-exact, never compositor-blurred (NFR-4.1).

### FR-5 — File operations

- **FR-5.1 Delete:** a keypress (`Delete`) or overlay button moves the
  current file to the freedesktop trash — **instantly, no dialog** — and
  advances to the next image.
- **FR-5.2 Undo:** after a delete, a toast with an **Undo** action is
  shown for ~5 seconds; activating it restores the file from trash and
  returns to it. `Ctrl+Z` triggers the same undo while the toast is up.
- **FR-5.3 Rotate view:** rotate the displayed image 90° CW/CCW without
  touching the file.
- **FR-5.4 Rotate + save:** an explicit save action persists the current
  rotation to disk:
  - JPEG: losslessly (no pixel re-encode).
  - PNG/WebP/AVIF/BMP: re-encoded at equivalent quality settings.
  - SVG and animated images: view-rotate only; save is disabled and the
    UI says why.
- **FR-5.5** Saves are atomic — a crash or power loss mid-save never
  leaves a corrupt or truncated file in place of the original.
- **FR-5.6** No other write operation exists. The app never modifies,
  moves, or creates files except FR-5.1/5.2/5.4.

### FR-6 — Window & UI

- **FR-6.1** Frameless window: no titlebar, no menubar — the image fills
  the window edge to edge over a dark background.
- **FR-6.2** Overlay controls (prev/next, rotate, delete, position/zoom
  indicator, close button) fade in on mouse movement and fade out after
  ~2 s of inactivity; they never appear during pure keyboard use.
- **FR-6.3** Fullscreen toggle via `F`/`F11` and double-click.
- **FR-6.4** Window can be moved by dragging the image (when not panning
  a zoomed image).
- **FR-6.5** Every mouse-reachable action has a keyboard equivalent and
  vice versa.
- **FR-6.6 Initial window size:** on open, the window sizes itself to the
  image at 100 % up to 85 % of the monitor's work area (larger images are
  fitted down to that bound); it never opens larger than the image
  itself. Subsequent images reuse the current window size (FR-4.6).
- **FR-6.7 Closing:** the app closes via `Q`, `Escape`, the overlay ×
  button, or the window-manager close request. In fullscreen, the first
  `Escape` exits fullscreen and the second closes; `Q` always closes
  immediately. Closing exits the process — nothing lingers (NFR-2.2).
  Close is never blocked by a prompt: any pending delete-undo window
  simply lapses (the file stays in trash, still recoverable via Files).

### FR-7 — Single instance

- **FR-7.1** Opening an image while a window is already open loads it
  into the existing window (raised/focused) instead of spawning a new
  process.

### FR-8 — Configuration

- **FR-8.1** Optional plain-text config at
  `~/.config/open-mpv/open-mpv.conf`, mpv-style `key=value` lines with
  `#` comments. Absent file ⇒ all defaults.
- **FR-8.2** Configurable at minimum: background color, sort order
  (name/date), navigation wrap, default fit mode, overlay fade delay,
  and keybindings (any action rebindable).
- **FR-8.3** Unknown or malformed lines are ignored with a warning on
  stderr — a bad config never prevents startup.

### FR-9 — Desktop integration

- **FR-9.1** Ships a `.desktop` entry and MIME associations for all
  FR-2 formats; installable as the system **default** image viewer so
  double-click in Files opens it.
- **FR-9.2** Shows a proper app icon and name in the GNOME shell
  (window switcher, dock).

---

## 2. Non-Functional Requirements

### NFR-1 — Performance

- **NFR-1.1 Startup:** cold launch to first visible image ≤ 300 ms for a
  typical 12 MP JPEG on the target machine; warm/single-instance open
  ≤ 100 ms.
- **NFR-1.2 Navigation:** next/previous displays the neighbor image
  ≤ 100 ms in the common case (neighbors are pre-decoded in the
  background).
- **NFR-1.3 Responsiveness:** the UI thread never blocks on decoding —
  even a 100 MP image or a broken file must leave the window responsive
  (input, resize, quit) at all times.
- **NFR-1.4 Interaction:** zoom, pan, and overlay animation hold the
  display's refresh rate for images up to 50 MP.

### NFR-2 — Footprint

- **NFR-2.1** Idle memory (one 12 MP photo shown) ≤ 150 MB RSS; memory
  is bounded while flipping through arbitrarily large folders (decoded
  neighbors capped, not accumulated).
- **NFR-2.2** No background daemon, no network access, no telemetry —
  the process exists only while a window is open.
- **NFR-2.3** Installed size (binary + assets, excluding shared system
  libraries) ≤ 15 MB; runtime dependencies limited to libraries already
  present on GNOME Workstation.

### NFR-3 — Reliability & data safety

- **NFR-3.1** The app must never corrupt or lose an image: deletes go to
  trash only, saves are atomic (FR-5.5), and no code path writes to a
  file it wasn't explicitly asked to.
- **NFR-3.2** Malformed/hostile image files must not crash the app or
  execute code — decoding runs sandboxed/isolated from the app process.
- **NFR-3.3** A decoder crash on one file shows the error state for that
  file; the app and folder navigation keep working.

### NFR-4 — Platform

- **NFR-4.1** First-class native Wayland client on GNOME (Fedora
  Workstation); no XWayland requirement. Fractional scaling and HiDPI
  render sharp.
- **NFR-4.2** Follows the system light/dark preference only where it has
  chrome (toasts, error states); the canvas stays the configured
  background color.
- **NFR-4.3** Works offline, from a read-only location, and on files the
  user can read but not write (write actions gray out, viewing is
  unaffected).

### NFR-5 — Usability

- **NFR-5.1** Zero learning curve for the basics: open, scroll, arrows,
  `Delete`, `F`, `Q` behave as any user would guess.
- **NFR-5.2** Discoverable: a `?` key shows a one-screen keybinding
  cheat-sheet overlay; nothing else needs documentation.
- **NFR-5.3** All destructive-adjacent feedback (delete toast, save
  confirmation, errors) is glanceable and non-modal — no dialog ever
  interrupts flipping through photos.

### NFR-6 — Maintainability & extensibility

- **NFR-6.1** The folder model (sorted file list, watching, navigation)
  is a separate module from the viewer widget, so the future explorer
  iteration can reuse it unchanged (see PLAN.md, Future direction).
- **NFR-6.2** Keybindings and actions go through a single action layer —
  the config file (FR-8) and future UI surfaces bind to the same
  actions.
- **NFR-6.3** The project builds with stock Rust tooling (`cargo build`)
  on Fedora with system GTK4 packages; no vendored toolchains.
