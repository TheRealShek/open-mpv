# Working on open-mpv

open-mpv is a fast, minimal viewer for local photos and videos. It is built for
Fedora Workstation, GNOME and Wayland with Rust, GTK4, Glycin, GStreamer and
GIO.

The application should feel immediate and stay out of the user's way. It does
not manage a library, use the network or take ownership of the user's files.
Images and videos are equally important.

## Product direction

Keep these ideas in mind when making decisions:

- Opening and moving through media should remain fast, including for large or
  broken files.
- Viewing is safe by default. Source files change only after a clear user
  action.
- Show controls only when they are useful. Ordinary viewing should not be
  crowded by advanced controls.
- The application owns one Workspace. Multiple windows, a media library and
  other large expansions need a product decision first.
- Fedora GNOME/Wayland is the Reference environment. Do not claim support for
  another platform, package or hardware path without the same level of testing.

The current priority is to make the Viewer solid before adding the Explorer.
Image Edit mode comes later and needs its own plan. Video editing, frame
stepping, slideshows, network playback, X11 support and Flatpak distribution
are not in the current direction.

## Sources of truth

Read the document that owns the decision before changing it:

- [README.md](README.md) explains the released product to users, including
  installation, controls and troubleshooting.
- [docs/CONFIGURATION.md](docs/CONFIGURATION.md) explains every setting,
  default, value and configurable action.
- [CONTEXT.md](CONTEXT.md) defines product language, boundaries and the meaning
  of terms such as Viewer, Explorer and Navigation set.
- [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) defines exact behavior,
  performance limits and the order in which the product should grow.
- [docs/DISTRIBUTION.md](docs/DISTRIBUTION.md) owns packaging, releases and
  supported-distribution decisions.
- Module documentation and tests own detailed code rules.

Do not copy a detailed rule into several documents. Update the document that
owns it, then link to it where another audience needs to find it.

Documentation is part of a change, not follow-up work. Update affected docs in
the same change without waiting for a separate request. Keep these files in
sync:

| Change | Also inspect and update |
| --- | --- |
| User-visible behavior or controls | `README.md` and the matching requirement |
| Configuration syntax, settings, defaults, values or actions | `docs/CONFIGURATION.md`, `src/config.rs`, its tests and FR-8 |
| Product scope, names or future direction | `CONTEXT.md` and `docs/REQUIREMENTS.md` |
| Supported media formats | `src/config.rs`, `README.md`, requirements, installer and desktop MIME metadata |
| Packaging, dependencies or supported systems | `docs/DISTRIBUTION.md`, installer, RPM and application metadata |
| Architecture or module ownership | module documentation and this file when the high-level map changes |

Every documented configuration example must remain valid input. If a change
makes a documented command, configuration line or promise untrue, update the
documentation in the same change.

## How the application fits together

GTK receives a request to open media and passes it to the state and typed
actions in `window`. `folder` supplies the ordered Navigation set. Images go
through `loader`; videos go through `player`. `viewer` displays both as a
`GdkPaintable`.

```text
open request -> window state/action -> folder
                                  -> loader or player -> viewer
```

Changes to source files take a separate, explicit path:

```text
user action -> typed window action -> fileops -> update folder state
```

Media work can finish after the user has moved to another file. Before using a
result, check that it still belongs to the active media.

## Module ownership

Keep each rule in the module that owns it:

| Module | Owns |
| --- | --- |
| `main` | GTK application lifecycle, activation and the single-instance entry point |
| `config` | defaults, parsing, supported extensions and key bindings |
| `folder` | selected folder, sorted Navigation set, current destination, generation and changes to the set |
| `loader` | background Glycin image decoding and the limited image cache |
| `annotation` | bounded Quick Markup shapes and shared preview/copy drawing |
| `viewer` | fit, zoom, pan, view rotation and source/view transforms for any paintable |
| `player` | GStreamer setup, playback, seeking and stream selection |
| `fileops` | trash, restore and rotate-save operations |
| `window` | UI composition, folder-monitor adapters, displayed media state, overlays and typed actions |
| `log` | timed diagnostic messages outside rendering paths |

`folder` must stay independent of GTK and GIO so Viewer and Explorer can share
the same model. Commands belong to the typed action layer in `window`; direct
view manipulation can stay in `viewer`.

## The easiest ways to break open-mpv

### Apply an old result to new media

Decode, animation, SVG, metadata and future Explorer preview work can finish
later. Cancel work when practical and reject results for media or cells that
are no longer current.

### Block GTK or let work grow forever

GTK's main loop is the only event loop. Keep decoding, enumeration, metadata
and file operations away from it, using the GLib integration already present.
Caches, queues, jobs, shapes, undo history, pipelines, file descriptors and
threads must have clear limits and settle after repeated use. Never log from a
snapshot or other render path.

### Write data the user did not ask to change

The application has no private history, library, state database or on-disk
media cache. It may save only the main optional configuration, standard
freedesktop thumbnails for Explorer, and source-file changes owned by
`fileops`. Quick Markup remains clipboard-only. Any other saved state needs a
product decision.

All file replacement must be safe and atomic. Trash, restore, unreadable files
and failed saves are normal failure paths, not reasons to panic.

### Create a second behavior path

Keys, menus and buttons for the same command must reach the same typed action.
When changing a command, inspect every way to trigger it, when it is available,
its configured binding, help text and tests. Gestures that directly pan or zoom
the view are the exception.

### Claim support without testing it

Use GTK, GIO, Glycin and GStreamer rather than adding X11 behavior, custom IPC
or another media stack. Hardware and packaging behavior must keep a safe
fallback and respect an explicit system disable. Test on the real environment
before claiming that a backend or platform is supported.

## Think across state changes

Think about how every mode starts and ends. Important transitions include
image to video, video to image, one media item to another, Viewer to Explorer,
normal viewing to Quick Markup, playback to error or close, and Trash to Undo.
Release resources, cancel late work and restore shared state on every relevant
exit path.

When changing file operations, include folder-monitor behavior and Undo. When
changing rendering or markup, include rotation, zoom, pan and fractional
scale. When changing playback, include navigation, pipeline cleanup, missing
codecs and software fallback. Tests should cover the decision and its important
failure cases rather than merely copy the implementation.

Before adding a dependency or helper, look for a tool or implementation that
already exists. Add a dependency only when Rust, GTK, GIO, Glycin or GStreamer
cannot provide a clear and safe solution.

## Verification

Start with the smallest check that proves the changed behavior. When a code or
packaging change is ready, run every relevant check:

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

`cargo test` performs real trash and rotate-save operations and needs a user
session and ImageMagick. Report checks as passed, failed, skipped or inspected.
Do not hide an unresolved failure by installing or shipping the build.

Some promises require more than automated tests:

- Human GNOME/Wayland testing covers keyboard, pointer, clipboard, window and
  visual behavior.
- Performance work is measured against the limits in the requirements. Use
  PSS rather than RSS for memory.
- Hardware decoding is verified on the real device, including alternate,
  disabled and software-fallback paths.
- Packaging changes include install, update, removal and desktop integration.

Do not install a build by default. When human testing is needed, first finish
the automated checks, prepare the relevant build, and report its path, run
command and a short manual checklist. Install only when the task requires it
or the user asks.

## Git and releases

Do not commit, push, open a pull request, merge or publish a release unless the
user asks. Never overwrite unrelated work in a dirty worktree. `main` is
protected; requested GitHub work uses a focused branch and the required CI.

A major change is not ready to merge or release until the author has tested
that exact change or has clearly approved it without personal testing.
State what automated checks passed, what still needs human testing and the
remaining risk. Follow [docs/DISTRIBUTION.md](docs/DISTRIBUTION.md) for every
packaging or release decision.
