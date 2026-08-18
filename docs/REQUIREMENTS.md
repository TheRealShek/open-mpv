# open-mpv — Requirements

This is the authoritative specification of testable product behavior and
budgets. [PLAN.md](PLAN.md) owns product intent, scope and exclusions. "Must"
requirements define the finished product; there is no version staging.

---

## 1. Functional Requirements

### FR-1 — Opening media

- **FR-1.1** The app opens local supported media from `open-mpv <path>`, a
  desktop "Open with" action, drag-and-drop or the in-app **Open File…**
  action. Every entry point uses the same opening behavior.
- **FR-1.2** Opening a file implicitly loads its folder context: all
  supported media in the same directory become the navigation set.
- **FR-1.3** Opening a directory path or choosing **Open Folder…** shows the
  first supported item in that directory (same sort order as FR-3.2).
- **FR-1.4** A missing or unreadable path shows a clear in-window error, never
  a crash or silent exit.
- **FR-1.5** Launching with no argument opens an empty window with keyboard-
  accessible **Open File…** and **Open Folder…** controls and accepts
  drag-and-drop of one file.
- **FR-1.6 In-app opening:** **Open File…** uses `Ctrl+O`; **Open Folder…**
  uses `Ctrl+Shift+O`. Both actions are rebindable (FR-8.2), appear in the
  More/secondary-click menu and remain available from empty and error states.
  The file chooser selects one file and shows only FR-2/FR-10 media types.
  Choosers start in the current media folder when one exists, otherwise leave
  location memory to the desktop portal. Cancelling either chooser leaves the
  current media unchanged; chooser failures are reported non-modally.

### FR-2 — Supported formats

- **FR-2.1** Still images: every format the installed glycin loaders can
  decode — JPEG, PNG, WebP, AVIF, BMP, HEIF/HEIC, JPEG XL, TIFF, JPEG
  2000, ICO, TGA, QOI, OpenEXR, DDS, PNM/PBM/PGM/PPM, XBM, XPM. Folder
  filtering and desktop MIME registration remain aligned with that set.
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
- **FR-3.4** The top-left information overlay shows the current filename
  and position ("17 of 244").
- **FR-3.5** The navigation set tracks files added, removed or renamed while
  viewing.

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
- **FR-4.7 Scaling quality:** downscaling uses mipmapped/trilinear filtering
  without aliasing or shimmer; upscaling uses smooth interpolation. At HiDPI
  and fractional scales, 100 % maps pixel-exactly to physical pixels rather
  than being compositor-blurred (NFR-4.1).

### FR-5 — File operations

- **FR-5.1 Delete:** `Delete` or the overlay action immediately moves the
  current file to freedesktop trash without a dialog and advances.
- **FR-5.2 Undo:** for ~5 seconds after deletion, the toast's Undo action or
  `Ctrl+Z` restores the file and returns to it.
- **FR-5.3 Rotate view:** rotate the displayed image 90° CW/CCW without
  touching the file.
- **FR-5.4 Rotate + save:** an explicit save action persists the current
  rotation to disk:
  - JPEG: losslessly (no pixel re-encode).
  - PNG/WebP/AVIF/BMP: re-encoded at equivalent quality settings.
  - SVG and animated images: view-rotate only; no save control is offered.
  Show Save only for a writable image with a pending supported rotation.
- **FR-5.5** Saves are atomic — a crash or power loss mid-save never
  leaves a corrupt or truncated file in place of the original.
- **FR-5.6** No other persistent file operation exists. The app never modifies,
  moves, or creates files except FR-5.1/5.2/5.4. FR-11 publishes a transient
  clipboard image but never writes it to disk.

### FR-6 — Window & UI

- **FR-6.1** The frameless window has no titlebar or menubar; media fills it
  edge-to-edge over a dark background.
- **FR-6.2** The overlay places filename and position top-left,
  fullscreen/close top-right, previous/next at the sides and media actions
  bottom-centre. Pointer motion reveals it; ~2 seconds of inactivity hides it,
  but keyboard use never reveals it and a hovered control or open More menu
  holds it open. `hide-cursor` hides the pointer with the overlay except on the
  FR-6.4 resize border.
  Quick Markup replaces the normal bottom controls with its focused toolbar,
  which remains visible until that mode ends.
- **FR-6.3** Fullscreen toggle via `F`/`F11` and double-click.
- **FR-6.4** Window can be moved by dragging the image (when not panning
  a zoomed image or using Quick Markup) and resized by dragging within a few
  pixels of any edge or corner, with the pointer showing the resize cursor
  there.
- **FR-6.5** Every action is reachable from the keyboard and rebindable
  (FR-8.2). The overlay keeps the current medium's primary controls
  visible and puts Open File, Open Folder, Fit, Actual Size, rotate, First,
  Last, Quick Markup and keyboard help in one menu, opened from either the
  bottom More button or a secondary click on the medium.
- **FR-6.6 Initial window size:** open at 100 % media size up to 85 % of the
  monitor work area, never larger than the media. Video starts at a default
  size and resizes once after preroll provides its dimensions. Subsequent media
  reuse the current window size (FR-4.6).
- **FR-6.7 Closing:** the app closes via `Q`, `Escape`, the overlay ×
  button or the window manager. In fullscreen, the first `Escape` exits
  fullscreen and the second closes; `Q` closes immediately. Closing leaves no
  process (NFR-2.2) and no prompt; pending undo simply lapses with the file in
  trash. In Quick Markup, `Escape` first cancels an active draft or exits that
  mode; `Q` and the window manager still close immediately and discard it.

### FR-7 — Single instance

- **FR-7.1** Opening an image while a window is already open loads it
  into the existing window (raised/focused) instead of spawning a new
  process.

### FR-8 — Configuration

- **FR-8.1** Optional plain-text config at
  `~/.config/open-mpv/open-mpv.conf`, mpv-style `key=value` lines with
  `#` comments. Absent file ⇒ all defaults.
- **FR-8.2** Configurable at minimum: background color, sort order
  (name/date) and direction, navigation wrap, default fit mode, overlay
  fade delay, pointer hiding, video looping, starting volume, opening
  fullscreen, initial subtitle mode (`auto`/`off`), and keybindings (any
  action rebindable; `bind=<key> none` removes a default rather than
  overriding it).
- **FR-8.3** Unknown or malformed lines are ignored with a warning on
  stderr — a bad config never prevents startup.

### FR-9 — Desktop integration

- **FR-9.1** Ships a `.desktop` entry and MIME associations for all
  FR-2 image formats and FR-10.1 video containers; installable as the
  system **default** viewer for both so double-click in Files opens it.
- **FR-9.2** Shows a proper app icon and name in the GNOME shell
  (window switcher, dock).

### FR-10 — Video playback

- **FR-10.1** Plays MP4, MKV, WebM, MOV and AVI through system GStreamer
  (`playbin3` → `gtk4paintablesink`). Compatible streams prefer VA-API and
  reach GTK as dmabufs without CPU pixel copies; streams beyond hardware codec
  or dimension limits may use an installed software decoder. GStreamer starts
  lazily on the first video to preserve NFR-1.1.
- **FR-10.2** Videos appear in the same folder navigation as images
  (FR-3). They are streamed on show, never pre-decoded into the
  neighbor cache.
- **FR-10.3** Playback loops at end of stream, like animated images —
  configurable, with `loop=no` leaving the last frame up.
- **FR-10.4** Rebindable transport uses the single action layer (FR-8.2):
  play-pause (advance on images), ±10-second seek through `J`/`L` or
  `Shift+Left`/`Shift+Right`, mute and volume up/down. Plain horizontal arrows
  still navigate or pan. Zoom, pan and view rotation apply to video, but save
  does not. Fit scales video up or down to the largest aspect-preserving window
  area, filling matching-aspect fullscreen.
- **FR-10.4a Playback speed:** local videos offer 0.5×, 0.75×, 1×, 1.25×,
  1.5× and 2× presets through a contextual transport menu and rebindable
  slower, faster and reset actions (`[` / `]` / `\`). A rate change preserves
  position and play/pause state, keeps seeking, scrubbing, looping and
  subtitles synchronized, and briefly shows the selected rate. Every newly
  opened video starts at 1×. Audio keeps its pitch through GStreamer's
  `scaletempo`; when that optional element or a compatible rate seek is
  unavailable, the current rate is retained and a non-modal message explains
  why.
- **FR-10.5** For video, the bottom overlay shows play/pause, a seek bar,
  `position / duration`, playback speed and mute, polling position only while
  visible. The seek bar is at most 320 px and shrinks first; on narrow windows,
  duplicate time, mute, subtitles and playback speed yield before play/pause,
  Trash or the seek target.
- **FR-10.6** Missing plugins or a failing pipeline are routine states:
  in-window error message, never a crash. Unlike images (NFR-3.2), video
  decoding runs in-process.
- **FR-10.7 Subtitles:** GStreamer renders embedded tracks and local
  SRT/WebVTT. On video open, discover same-directory sidecars named for the
  video stem plus optional dot-separated language/role components; never add
  them to FR-3 navigation. Automatic selection respects container defaults and
  a matching sidecar. Every video's CC and More/right-click subtitle menus
  offer Add External Subtitle; when tracks exist, also offer Automatic, Off
  and every track. Add uses a native SRT/WebVTT-filtered chooser. `V` toggles
  visibility and `Shift+V` cycles tracks through the action layer. Dropping a
  local subtitle from Files onto the playing video attaches and selects it,
  replacing any prior external attachment; navigation forgets it. Subtitle
  errors are non-modal and never stop otherwise playable video.

### FR-11 — Quick Markup

- **FR-11.1 Availability:** a decoded static raster image offers Quick Markup
  through `A`, the photo controls and the More/secondary-click menu. Animated
  images, SVG, video, loading, empty and error states do not offer it.
- **FR-11.2 Focused controls:** entering Quick Markup replaces the normal
  bottom controls with persistent Box, Arrow, Undo, Clear, Cancel and Copy
  controls. Box is selected initially. `B` selects Box, `Shift+A` selects
  Arrow, `C` clears and `Ctrl+C` copies; every command uses the FR-8.2 action
  layer. The tools use one fixed high-visibility red style with a contrasting
  light edge rather than exposing editing preferences.
- **FR-11.3 Image-relative geometry:** a primary-button drag starting inside
  the image creates the selected shape and clamps its endpoint to the image.
  Shapes remain attached to decoded-image pixel coordinates through fit,
  zoom, pan, resize, view rotation, HiDPI and fractional scaling. Tiny clicks
  do not create shapes.
- **FR-11.4 Mode isolation:** primary drag draws instead of panning or moving
  the window. Folder navigation, Trash, rotate, rotate-save, in-app Open,
  secondary-click and double-click fullscreen are suppressed until Copy or
  Cancel; horizontal scroll cannot navigate. Scroll/pinch zoom, arrow-key pan,
  explicit zoom actions, `F`/`F11`, `Q` and resize edges continue working. A
  resize cursor wins at an edge, a crosshair identifies the drawing surface,
  and controls retain the normal pointer.
- **FR-11.5 Transient history:** `Ctrl+Z` undoes the active draft or most recent
  shape instead of trash while Quick Markup is active. Clear removes all
  shapes as one undoable operation, even after a cancelled or tiny subsequent
  drag. The canvas holds at most 128 shapes; undo storage is also bounded and
  never persists beyond the current session.
- **FR-11.6 Clipboard result:** Copy is enabled only after a completed shape.
  It publishes a bitmap containing the complete decoded image and annotations
  at the image's native pixel dimensions, with current view rotation baked in.
  It excludes zoom, pan, canvas, window chrome and source metadata. Success
  exits the mode and reports non-modally; failure keeps the session available
  and reports the error non-modally.
- **FR-11.7 Lifecycle:** `A`, Cancel or `Escape` discards the transient session
  without a dialog; an in-progress draft consumes the first `Escape`. Opening
  another path through drag-and-drop or single-instance activation, losing the
  current file, or closing the app also discards it. Clipboard persistence
  after open-mpv exits is left to the Wayland compositor and is not promised.
- **FR-11.8 Data safety:** Quick Markup never changes the source, creates a
  file, adds a folder entry, or enters `fileops`.

---

## 2. Non-Functional Requirements

### NFR-1 — Performance

- **NFR-1.1 Startup:** cold launch to first visible image ≤ 300 ms for a
  typical 12 MP JPEG on the target machine; warm/single-instance open
  ≤ 100 ms.
- **NFR-1.2 Navigation:** next/previous displays the neighbor image
  ≤ 100 ms in the common case (neighbors are pre-decoded in the
  background).
- **NFR-1.3 Responsiveness:** decoding never blocks the UI thread; input,
  resize and quit remain responsive for a 100 MP image or broken file.
- **NFR-1.4 Interaction:** zoom, pan, and overlay animation hold the
  display's refresh rate for images up to 50 MP. Quick Markup preview adds
  only bounded vector geometry to that render path; bitmap composition occurs
  only when Copy is explicitly requested.

### NFR-2 — Footprint

- **NFR-2.1 Idle memory:** one 12 MP photo shown, ≤ 100 MB **PSS**. Use
  PSS rather than RSS so shared GTK and Mesa pages are apportioned correctly.
- **NFR-2.1a** Memory is bounded while flipping through arbitrarily
  large folders: decoded neighbours are capped, not accumulated, and
  memory, file descriptors, threads and loader processes settle after
  repeated navigation and video/image cycles.
- **NFR-2.1b** Quick Markup retains at most 128 visible vector shapes and 256
  shape records across bounded undo snapshots. Copy may retain one additional
  native-size texture as the current clipboard payload; replacing the
  clipboard payload releases the prior result.
- **NFR-2.2** No background daemon, no network access, no telemetry —
  the process exists only while a window is open.
- **NFR-2.3** Binary and assets total ≤ 15 MB, excluding shared system
  libraries; runtime dependencies are limited to libraries already present on
  GNOME Workstation.

### NFR-3 — Reliability & data safety

- **NFR-3.1** Never corrupt or lose an image: delete only to trash, save
  atomically (FR-5.5), write only when explicitly requested, and never write
  Quick Markup into the source.
- **NFR-3.2** Malformed/hostile image files must not crash the app or
  execute code — decoding runs sandboxed/isolated from the app process.
- **NFR-3.3** A decoder crash on one file shows the error state for that
  file; the app and folder navigation keep working.

### NFR-4 — Platform

- **NFR-4.1** First-class native Wayland client on GNOME (Fedora
  Workstation); no XWayland requirement. Fractional scaling and HiDPI
  render sharp.
- **NFR-4.2** Chrome (overlay controls, toasts, error states) is always
  a dark translucent OSD independent of the system light/dark preference.
  The canvas stays the configured background color.
- **NFR-4.3** Works offline, from a read-only location, and on files the
  user can read but not write. Viewing is unaffected; a write that
  cannot succeed reports a toast rather than being pre-emptively disabled.

### NFR-5 — Usability

- **NFR-5.1** Zero learning curve for the basics: open, scroll, arrows,
  `Delete`, `F`, `Q` behave as any user would guess.
- **NFR-5.2** Discoverable: a `?` key shows a one-screen keybinding
  cheat-sheet overlay, including Quick Markup controls; nothing else needs
  documentation.
- **NFR-5.3** All destructive-adjacent feedback (delete toast, save
  confirmation, errors) is glanceable and non-modal — no dialog ever
  interrupts flipping through photos.

### NFR-6 — Maintainability & extensibility

- **NFR-6.1** The folder model (sorted file list and navigation) remains
  reusable by another UI surface without GTK or GIO dependencies; file
  watching integrates without entering that model (see
  [PLAN.md](PLAN.md#future-direction-not-in-scope-informs-design)).
- **NFR-6.2** Keybindings and actions go through a single action layer —
  the config file (FR-8) and future UI surfaces bind to the same
  actions.
- **NFR-6.3** The project builds with stock Rust tooling (`cargo build`)
  on Fedora with system GTK4 packages; no vendored toolchains.
