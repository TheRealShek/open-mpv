# open-mpv — Requirements

[../CONTEXT.md](../CONTEXT.md) defines the product language and boundaries.
This document contains only behavior and quality promises that can be verified.

## Delivery direction

Work proceeds in this order:

1. make the existing Viewer's safety, correctness, performance and
   documentation promises demonstrably true;
2. add the Explorer without regressing the Viewer; and
3. plan an image-only Edit mode separately before implementing it.

Video editing, frame stepping, slideshow automation, multiple windows, X11
support and Flatpak distribution are not in the current direction. They require
new product decisions rather than incidental implementation.

## Functional requirements

### FR-1 — Opening media and workspace

- **FR-1.1 / FR-1.6** Every entry point uses the same typed open actions: command line, desktop
  Open With, drag-and-drop, single-instance activation and in-app choosers.
- **FR-1.2 / FR-1.3** Opening a supported file enters the Viewer with its containing folder as the
  Navigation set. Opening a folder enters the Explorer for that folder.
- **FR-1.5** Starting without a path shows an empty Workspace with accessible Open File
  and Open Folder actions. Cancelling a chooser changes nothing.
- **FR-1.4** Missing, unreadable, unsupported or corrupt direct targets show a clear
  in-window error. Routine failures never crash or silently close the app.
- **FR-7.1** One active Workspace is reused by later activations. Multiple windows are
  not part of the current product.

### FR-2 — Supported local media

- **FR-2.1–FR-2.4** Images include the formats supported and registered by the installed Glycin
  loaders, including JPEG, PNG, WebP, AVIF, HEIF/HEIC, JPEG XL, TIFF, SVG, GIF
  and animated WebP/APNG. Orientation metadata is respected.
- **FR-10.1 / FR-10.2** Videos include MP4, MKV, WebM, MOV and AVI through installed GStreamer
  plugins. Images and videos are equally central Local media.
- **FR-2.1 / FR-9.1** Static extension filtering, desktop MIME registration, installation
  dependencies and tested decoder support remain aligned.
- **FR-10.7** Local SRT and WebVTT files may attach to video but never enter the Navigation
  set.

### FR-3 — Navigation set

- **FR-3.2** The Navigation set is the supported images and videos in one folder, in one
  shared order. It is never split by media type.
- **FR-3.2 / FR-3.3** Natural case-insensitive filename order is the default; modification-date
  order, reverse order and wrapping are configurable.
- **FR-3.1 / FR-3.3** Next, Previous, First and Last use the same typed actions from keys, controls
  and gestures. Reaching an end without wrapping gives subtle feedback.
- **FR-3.4 / FR-3.5** Folder additions, removals and renames update the set while preserving the
  logical current item. Removing the current item lands on its nearest
  remaining position rather than the first item.
- **FR-2.5** Directly opened broken media shows its error; navigation may skip broken
  items with bounded work and must never loop forever.

### FR-4 — Viewer

- **FR-2.1–FR-2.3** The Viewer renders still images, animated images, sharp rerendered SVG and
  video through one visual surface.
- **FR-4.1 / FR-4.3 / FR-4.7** Images fit down to the window without being enlarged by default. Video may
  fit up or down. Actual size maps one media pixel to one physical pixel at
  HiDPI and fractional scales.
- **FR-4.2–FR-4.4** Zoom spans at least 5%–2000%, anchors pointer and pinch input correctly, and
  supports pan, fit, actual size and quarter-turn view rotation.
- **FR-4.5 / FR-4.6** Navigation resets the view; resize preserves fit or manual zoom semantics.
  Pan state is clamped so no hidden overshoot accumulates.
- **FR-2.2** Animated images play automatically and loop. Space and a contextual control
  pause or resume them without restarting the animation.
- **NFR-1.3 / NFR-3.3** Async decode, animation, SVG and metadata results apply only to the media
  generation that started them.

### FR-5 — File, clipboard and editing safety

- **FR-5.1 / FR-5.2** Trash immediately moves the current item to freedesktop trash and offers a
  short Undo. Undo never overwrites a path recreated after deletion and never
  mutates an unrelated folder or media generation.
- **FR-5.3 / FR-5.4 / FR-5.5** Explicit rotate-save supports only editable static images. JPEG remains
  lossless; every save path is atomic and preserves either the complete old or
  complete new file across interruption.
- **FR-5.6** No source media changes automatically. Current source writes are limited to
  explicit Trash, Restore and Rotate Save.
- **FR-5.6** Copy creates no file. In Viewer it copies a static image, intrinsic-size SVG
  or current animated-image frame with view rotation but without zoom, pan or
  chrome. Video-frame capture is outside the current direction.
- **FR-5.7** Future Edit mode begins with images only. Editing sessions remain transient
  and reversible until an explicit commit; Save a Copy is primary and Replace
  Original is a separate explicit action. Detailed editing operations require
  a later requirements decision.

### FR-6 — Window and interaction

- **FR-6.1 / FR-6.2** Media fills one frameless dark Workspace. Contextual chrome appears on
  pointer movement, stays for hover/open menus or focused modes, and hides
  after the configured delay. Keyboard use does not reveal it.
- **FR-6.2 / FR-6.5** Viewer, Explorer and future Edit mode use Progressive disclosure rather than
  permanent advanced toolbars.
- **FR-6.5 / NFR-6.2** Every command is keyboard reachable, rebindable and routed through the typed
  action layer. Direct manipulation gestures may manipulate the view directly;
  command-equivalent clicks use the action layer.
- **FR-6.3 / FR-6.4 / FR-6.6** Fullscreen, compositor-owned move/resize, edge cursors and pointer hiding
  work natively on Wayland. Initial size uses the active monitor work area.
- **FR-6.7** Escape unwinds the active draft, focused mode, Explorer destination and
  fullscreen before closing. Quit closes immediately and leaves no process.
- **NFR-5.2** A generated help surface documents every active action and binding.

### FR-7 — Single application workspace

- **FR-7.1** The permanent application ID is `io.github.TheRealShek.OpenMpv`.
- **FR-7.1** GTK application activation provides one process and one active Workspace.
  Opening another path replaces the current media and raises the Workspace.
- **FR-7.2** Multiple windows or side-by-side comparison require a future product
  decision and must not be introduced accidentally.

### FR-8 — Configuration and Preferences

- **FR-8.1** The canonical optional config is
  `~/.config/open-mpv/open-mpv.conf`, using `key=value` lines and whole-line
  `#` comments. README examples must parse exactly as shown.
- **FR-8.2** Supported settings include background, sort, reverse, wrap, initial fit,
  overlay delay, pointer hiding, video looping, volume, fullscreen, subtitles,
  video previews, cache policy and typed keybindings. `none` removes a default
  binding.
- **FR-8.3** Unknown or malformed content warns on stderr and never prevents startup.
- **FR-8.4** Preferences is a later graphical wrapper over this same file for common
  settings. Changes apply immediately through atomic updates while preserving
  comments, ordering, keybindings, advanced settings and unknown content.
  Preferences never creates a second settings store.

### FR-9 — Desktop and native distribution

- **FR-9.1 / FR-9.2** The desktop entry, icon, AppStream metadata and MIME associations identify a
  local-media viewer and remain aligned with supported formats.
- **FR-9.3** Native packages are preferred. The verified GitHub RPM remains the initial
  path; Copr is the next Fedora update channel, followed by evaluation of the
  official Fedora repository.
- **FR-9.4** An environment is called supported only after its complete automated,
  packaging, performance and human desktop checks pass. Fedora GNOME/Wayland
  is the Reference environment; community native packages elsewhere are
  welcome without implying official support.
- **FR-9.5** Packaging never changes default applications without an explicit user
  choice and never adds an in-app updater.

### FR-10 — Focused video playback

- **FR-10.1 / FR-10.2** GStreamer initializes only when the first video opens. Videos stream on
  demand, never enter the decoded-image cache, and release playback resources
  when an image is shown or the Workspace closes.
- **FR-10.3 / FR-10.4 / FR-10.4a / FR-10.5** Contextual controls provide play/pause, accurate seek and scrub, duration,
  volume/mute, looping and 0.5×–2× pitch-preserving playback presets.
- **FR-10.7** Embedded and local SRT/WebVTT subtitles support Automatic, Off and track
  selection. External attachment is local, non-modal and forgotten on
  navigation.
- **FR-10.8** Audio-track selection appears only when a video contains multiple tracks and
  remains in the secondary menu rather than normal transport chrome.
- **FR-10.1** Compatible installed hardware decoding is preferred using a policy verified
  on the Reference environment; unsupported streams may fall back to installed
  software decoding. Requirements do not mandate one vendor backend.
- **FR-10.6** Missing codecs, refused seeks/rates and subtitle errors are normal in-window
  errors. Playback failure never crashes the app.
- **FR-10.9** Frame stepping, playlists, network playback, subtitle downloading and video
  editing are outside the current direction.

### FR-11 — Quick Markup

- **FR-11.1 / FR-11.2** Quick Markup is available only for decoded static raster images and is
  deliberately entered through the typed action layer.
- **FR-11.3** Box and Arrow use image-relative coordinates and remain attached through
  fit, zoom, pan, resize, rotation and fractional scaling.
- **FR-11.2 / FR-11.4** Its focused toolbar exposes tool choice, Undo, Clear, Cancel and Copy while
  blocking navigation and source-changing actions. View manipulation,
  fullscreen, resize and Quit remain available.
- **FR-11.5** Shape count and undo history are bounded. Preview and Copy share drawing
  behavior.
- **FR-11.6–FR-11.8** Copy publishes the complete native-size annotated image with view rotation,
  then exits on success. Quick Markup never writes a file or persists a
  session.

### FR-12 — Explorer

- **FR-12.1** Opening a folder enters a virtualized grid for that folder only. Explorer
  has no directory tree, recursion, library, catalog or multi-selection.
- **FR-12.2** The complete lightweight folder list is enumerated off the GTK thread and
  sorted before presentation. Preview loading is progressive: only visible
  items and a small nearby scroll buffer materialize decoded textures.
- **FR-12.3** Every tile shows filename and a corner Media-type badge. Images use static
  thumbnails; videos use a static representative frame by default.
  `video-previews=no` performs no video-frame decoding and shows a neutral tile
  with the video badge instead.
- **FR-12.4** Standard freedesktop thumbnails are reused on disk. Explorer working memory,
  preview jobs and decoded textures are strictly bounded independently of
  folder size; leaving Explorer cancels work and releases its textures.
- **FR-12.5** The first item is selected and keyboard-focused without opening it. One
  click selects; double-click or Enter opens Viewer. Trash and Undo use the
  same actions as Viewer.
- **FR-12.6** A transient Filename filter has no index or persistence. Opening a filtered
  result temporarily narrows Next/Previous to those results until the filter
  is cleared.
- **FR-12.7** Opening an item preserves the Explorer session as a Back destination,
  including filter, selection and scroll position. Back or Escape returns to
  it.
- **FR-12.8** Preview failure shows a stable fallback tile and never blocks browsing.

### FR-13 — Information and handoff

- **FR-13.1** A deliberately opened read-only Info panel shows relevant facts already
  available from the filesystem or media pipeline and gathers expensive
  metadata lazily. It is not permanent chrome or a raw metadata dump.
- **FR-13.2** Contextual actions reveal the current item in the desktop file manager or
  ask another installed application to open it. Cancellation is silent and
  errors are non-modal.

## Non-functional requirements

### NFR-1 — Performance and bounded work

- **NFR-1.1 / NFR-1.2** Reference targets remain: cold launch to a typical 12 MP JPEG within 300 ms,
  warm/single-instance open within 100 ms, and common cached-neighbor
  navigation within 100 ms.
- **NFR-1.3** Decode, folder enumeration, metadata and file operations never block GTK's
  main loop. Input, resize and quit remain responsive for huge or broken media.
- **NFR-2.1 / NFR-2.1a** The displayed image may exceed a cache budget, but all additional decoded
  media is bounded by checked byte and count limits. A zero neighbor budget
  retains no neighbors.
- **NFR-1.4 / NFR-2.1a** Explorer memory is proportional to its viewport, not folder size. Preview
  concurrency and in-flight work are bounded and stale recycled-cell results
  are discarded.
- **NFR-1.4** Rendering contains no diagnostic I/O. Zoom, pan, playback and overlay work
  target the display refresh rate on the Reference environment.

### NFR-2 — Footprint

- **NFR-2.1** One typical 12 MP image targets at most 100 MB PSS. Measure PSS rather than
  RSS and recheck after startup, cache, pipeline or Explorer changes.
- **NFR-2.1a / NFR-2.1b** Decoded caches, vector shapes, undo records, textures, pipelines, file
  descriptors, threads and loader processes settle after repeated use.
- **NFR-2.3** Binary and bundled assets target 15 MB or less, excluding shared native
  libraries.

### NFR-3 — Reliability, privacy and data safety

- **NFR-3.3** Panics are bugs. Broken files, invalid config, unavailable codecs, malformed
  trash metadata and failed file operations return normal errors.
- **NFR-3.2** Glycin image decoding remains sandboxed. Video decoding remains in-process
  through the native GStreamer stack.
- **NFR-3.1** File replacement and generated cache entries use safe, atomic writes and do
  not follow predictable attacker-controlled temporary paths.
- **NFR-2.2** open-mpv has no account, telemetry, network media, background service or
  persistent library. Standard desktop thumbnails and explicit config/media
  saves are the only accepted persistent state beyond packaging.

### NFR-4 — Native platform behavior

- **NFR-4.1** Fedora GNOME/Wayland is the Reference environment. Fractional scale, HiDPI,
  compositor move/resize, clipboard and desktop portals work without X11
  assumptions.
- **NFR-4.2** Chrome remains readable dark translucent OSD independently of desktop theme;
  the canvas uses the configured background.
- **NFR-4.3** Viewing works offline, from read-only locations and without source write
  permission. Unavailable writes fail non-modally when explicitly attempted.

### NFR-5 — Usability and accessibility

- **NFR-5.1** Opening, navigation, zoom, playback, Trash, fullscreen and Quit behave
  predictably without configuration or documentation.
- **NFR-5.2** Keyboard and pointer are first-class. Contextual controls expose accessible
  names and the help surface reflects configured bindings.
- **NFR-5.3** Errors, saves and destructive-adjacent feedback are glanceable and
  non-modal. Only leaving a future Editing session with uncommitted changes
  may require an explicit discard decision.

### NFR-6 — Maintainability and verification

- **NFR-6.1** `folder` remains plain Rust without GTK or GIO so Viewer and Explorer share
  one ordering and navigation model.
- **NFR-6.1 / NFR-6.3** GTK's main loop is the only event loop. Async results carry the identity or
  generation needed to reject stale work.
- **NFR-6.2** User commands use one typed action vocabulary. Direct manipulation does not
  duplicate command policy.
- Dependencies require evidence that the Rust standard library, GTK, GIO,
  Glycin or GStreamer cannot provide the behavior safely.
- Automated tests cover decisions, important failures, stale results and
  resource bounds. GNOME/Wayland interaction, performance, hardware decoding
  and packaging receive explicit human or measured verification before support
  is claimed.
