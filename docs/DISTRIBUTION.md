# Distribution and updates

This document owns packaging, release and update decisions. It is a decision
guide, not a promise that the listed packages exist today. Product behavior
and scope remain authoritative in [REQUIREMENTS.md](REQUIREMENTS.md) and
[PLAN.md](PLAN.md).

## The outcome we want

A user should be able to:

1. install open-mpv without installing Rust or compiling source;
2. receive updates through the same trusted tool that installed it;
3. remove it cleanly;
4. choose whether it becomes the default viewer; and
5. retain every applicable product requirement after packaging.

The application must not contain its own updater. Package managers own update
delivery and rollback, preserving NFR-2.2.

## Constraints that affect packaging

- The supported platform today is Fedora Workstation with GNOME on Wayland.
- The code currently requires GTK 4.22, glycin and its matching loader
  protocol, GStreamer, and `gtk4paintablesink`.
- A package must satisfy [REQUIREMENTS.md](REQUIREMENTS.md), including glycin
  isolation, the video decoding path, file operations, desktop integration and
  the performance budgets.
- The current `install.sh` deliberately changes default file associations.
  A package must not do that during installation; the user or desktop should
  make that choice.
- The project is licensed under the MIT License, allowing public
  redistribution.

## Options

| Method | Easy install and updates | Reach | Fit for open-mpv | Decision |
| --- | --- | --- | --- | --- |
| Fedora RPM in Copr | DNF | Fedora | Matches the target host stack | Start here |
| Flatpak repository | Flatpak | Desktop Linux | Needs sandbox and media-path validation | Evaluate second |
| Standalone archive | Manual | Theoretical | Leaves dynamic-library compatibility to users | Debug builds only |
| AppImage | Usually manual | Theoretical | Poor fit for system codecs and graphics drivers | Do not prioritize |
| Build from source | Manual rebuild | Developers | Current contributor workflow | Keep for contributors |

### Why Copr first

An RPM preserves the Fedora library and media stack used for development while
Copr supplies reproducible installation and DNF updates. Target only Fedora
releases with the required GTK and glycin versions; do not bundle core desktop
libraries merely to support older releases.

### Why Flatpak needs a prototype

Flatpak broadens reach but changes filesystem and runtime boundaries. Prototype
a disposable manifest on real Wayland and run the requirements with particular
attention to folder access and monitoring, trash/restore and atomic saves,
glycin loaders, codecs and VA-API/libav, configuration location,
single-instance activation, cold start, PSS and installed size.

The result should use the narrowest permissions that preserve the product. If
folder navigation or file operations require broad host filesystem access, make
that trade-off explicit before publishing.

At the last review on 12 August 2026, Flathub's requirements made this
AI-assisted repository ineligible without a discretionary exception. Recheck
the linked policy before any submission; absent eligibility or a confirmed
exception, use a project-controlled Flatpak repository instead.

## Permanent application ID

The permanent ID is `io.github.TheRealShek.OpenMpv`. GTK uses it for D-Bus
single-instance identity, and the desktop entry, icon, AppStream metadata and
any future Flatpak must match it. The repository's `open-mpv` spelling may
require separate ownership proof, but a hyphenated ID is avoided because GLib
discourages hyphens in application IDs.

Keep this ID stable after public packaging; changing it would strand desktop,
MIME and package data under the old identity.

## Release and update model

Use the semantic version from `Cargo.toml` in immutable signed tags such as
`v0.1.0`. Release notes cover user-visible changes, fixes, known issues and
configuration changes.

The update flow should be:

```text
version + changelog -> signed source tag -> automated package build
                    -> package repository -> DNF or Flatpak update
```

Build stable packages only from tags and retain at least the previous package
for downgrade.

Use no fixed calendar. Release meaningful improvements when ready; publish
security or data-safety fixes promptly with a plain impact statement.

## Work required before the first package

1. Add AppStream metadata, including a summary, description, screenshots,
   supported URLs, launchable desktop ID, releases, and developer name.
2. Make installation package-friendly: support staged installation into a
   supplied prefix, and separate installation from changing MIME defaults.
3. Add an RPM spec and build it in a clean Fedora environment.
4. Test install, upgrade, downgrade, uninstall, desktop launch, MIME handling,
   and all media/file-operation paths.
5. Publish a tagged test release, then enable a Copr repository for users.

## Decision checkpoints

After testing the first RPM, answer with measurements:

- Does the package work on every Fedora release we claim to support?
- Do upgrades preserve config and file associations?
- Are startup time, PSS, and video hardware decoding unchanged?
- Can one maintainer reliably publish a fix without manual machine state?
- Are users outside Fedora actually asking for a package?

Only then run the Flatpak prototype. Adopt Flatpak if it passes the functional
and performance checks without permissions that undermine the product. If it
does not, keep Fedora/Copr as the honest supported distribution instead of
shipping a cross-distribution package with reduced behavior.

## References

- [Fedora: publishing packages in Copr](https://docs.fedoraproject.org/en-US/quick-docs/publish-rpm-on-copr/)
- [GNOME: why Flatpak is recommended for GNOME apps](https://developer.gnome.org/documentation/introduction/flatpak.html)
- [Flatpak sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)
- [Flatpak repositories and updates](https://docs.flatpak.org/en/latest/repositories.html)
- [Flathub submission requirements](https://docs.flathub.org/docs/for-app-authors/requirements)
- [GLib application ID rules](https://docs.gtk.org/gio/type_func.Application.id_is_valid.html)
