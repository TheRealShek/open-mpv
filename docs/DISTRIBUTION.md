# Distribution and updates

This document records how open-mpv should become easy to install and keep up
to date. It is a decision guide, not a promise that the listed packages exist
today.

## The outcome we want

A user should be able to:

1. install open-mpv without installing Rust or compiling source;
2. receive updates through the same trusted tool that installed it;
3. remove it cleanly;
4. choose whether it becomes the default viewer; and
5. keep the current guarantees: native Wayland, fast startup, sandboxed image
   decoding, hardware video decoding, no network access, and safe file
   operations.

The application should not contain its own updater. Package managers already
handle download verification, rollback or downgrade workflows, and unattended
updates. Avoiding an in-app updater also preserves NFR-2.2: open-mpv itself
never needs network access or a background process.

## Constraints that affect packaging

- The supported platform today is Fedora Workstation with GNOME on Wayland.
- The code currently requires GTK 4.22, glycin and its matching loader
  protocol, GStreamer, and `gtk4paintablesink`.
- Image decoding must remain isolated through glycin (NFR-3.2).
- Video should retain working VA-API hardware decoding and the optional libav
  fallback (FR-10.1).
- Folder navigation, trash/restore, rotate-save, configuration, desktop file
  associations, and single-instance activation must behave the same after
  packaging.
- The current `install.sh` deliberately changes default file associations.
  A package must not do that during installation; the user or desktop should
  make that choice.
- The project is licensed under the MIT License, allowing public
  redistribution.

## Options

| Method | Easy install and updates | Reach | Fit for open-mpv | Decision |
| --- | --- | --- | --- | --- |
| Fedora RPM in Copr | Yes, through DNF | Fedora users | Best match for the current host libraries, codecs, and hardware path | Start here |
| Flatpak / a Flatpak repository | Yes, through Flatpak and software centers | Most desktop Linux distributions | Strong reach, but filesystem access, trash/restore, glycin loaders, codecs, and hardware decode need validation | Evaluate second |
| Standalone archive | Manual | Many distributions in theory | Dynamic GTK/glycin/GStreamer compatibility remains the user's problem; no automatic updates | Debug builds only |
| AppImage | Usually manual | Many distributions in theory | Awkward fit for system codecs, glycin loaders, desktop integration, and Wayland graphics drivers | Do not prioritize |
| Build from source | Rebuild manually | Developers | Works now but is not a consumer distribution method | Keep as a contributor path |

### Why Copr first

An RPM keeps open-mpv on the same Fedora library and media stack on which it is
developed and measured. A Copr repository makes installation reproducible and
lets normal DNF updates deliver new versions. It is the smallest step from the
current source install to a real user installation.

This first package should target only Fedora releases that provide the required
GTK and glycin versions. Supporting an older Fedora release by bundling core
desktop libraries would add risk and defeat the main advantage of an RPM.

### Why Flatpak needs a prototype

Flatpak is the strongest candidate for reaching users beyond Fedora, but it
changes the app's filesystem and runtime boundaries. Before choosing it, build
a disposable manifest and verify all of these on a real Wayland session:

- opening a file from Files and loading the rest of its folder;
- live folder updates;
- trash followed by undo/restore;
- atomic rotate-save for files outside the sandbox;
- loading every advertised image format through compatible glycin loaders;
- video playback, seeking, audio, VA-API decoding, and libav fallback;
- reading configuration from the expected Flatpak-specific location;
- single-instance activation and opening a second file; and
- cold-start, PSS, and installed-size budgets.

The result should use the narrowest permissions that preserve the product. If
folder navigation or file operations require broad host filesystem access, make
that trade-off explicit before publishing.

Flathub is not currently an available host for this project. Its requirements,
checked on 12 August 2026, reject applications containing AI-assisted code or
documentation except when a discretionary exception is granted to a mature,
well-maintained project. This documentation was produced with AI assistance.
Do not prepare or open a Flathub submission unless the policy changes or
Flathub confirms an exception. A technically successful Flatpak can instead be
published from a project-controlled repository, but that has less discovery
than Flathub.

## Permanent application ID

The application ID is an internal identity, not the name shown to users.
open-mpv can keep its current product name regardless of this choice. GTK uses
the ID as the application's D-Bus name, which is how a second launch finds the
existing window. The desktop entry, icon, AppStream metadata, and any future
Flatpak must use the same ID.

The permanent ID is `io.github.TheRealShek.OpenMpv`. It connects the identity
to the GitHub account that owns the repository and is easier to verify than an
unrelated domain. The repository is named
`open-mpv`, rather than `OpenMpv`, so a service that calculates the repository
URL directly from the ID may require separate proof of ownership. Using a
hyphen in the ID to mirror the repository would avoid that mismatch, but GLib
discourages hyphens because they cause trouble in related D-Bus object paths
and desktop specifications. The CamelCase product component is the safer
technical choice.

This ID must remain stable after the first public package. A later change would
leave old desktop entries, icons, MIME associations, and package data behind
and make the new build look like a different application.

## Release and update model

Use semantic versions from `Cargo.toml` and make immutable signed Git tags such
as `v0.1.0`. Each public release should have short release notes that explain
user-visible changes, fixes, known issues, and any configuration change.

The update flow should be:

```text
version + changelog -> signed source tag -> automated package build
                    -> package repository -> DNF or Flatpak update
```

Stable packages should build only from a tag, never directly from the moving
`main` branch. Package repositories should retain at least the previous build
so a broken release can be downgraded while a fixed version is prepared.

There is no need for a fixed release calendar. Publish when a user-visible
improvement or important fix is ready. For a security or data-safety issue,
publish a patch release promptly and explain its impact plainly.

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

After the first RPM is tested, answer these with measurements rather than
assumptions:

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
