# Distribution and updates

This document owns packaging, release and update decisions. Product behavior
and scope remain authoritative in [REQUIREMENTS.md](REQUIREMENTS.md) and the
[project context](../CONTEXT.md). A documented release command becomes usable
only after the first corresponding GitHub Release has been published.

## The outcome we want

A user should be able to:

1. install open-mpv without installing Rust or compiling source;
2. install a newer release through the same stable GitHub URL;
3. remove it cleanly;
4. choose whether it becomes the default viewer; and
5. retain every applicable product requirement after packaging.

The application must not contain its own updater. While the audience remains
small, checking for and installing updates is deliberately manual. DNF owns
the local transaction and rollback behavior; GitHub hosts the RPM.

## Constraints that affect packaging

- The packaged platform today is Fedora 44 Workstation on x86-64 with GNOME
  and Wayland.
- The code currently requires GTK 4.22, glycin and its matching loader
  protocol, GStreamer, and `gtk4paintablesink`.
- A package must satisfy [REQUIREMENTS.md](REQUIREMENTS.md), including glycin
  isolation, the video decoding path, file operations, desktop integration and
  the performance budgets.
- Neither the RPM nor the default source installation changes file
  associations. `install.sh --set-default` is an explicit convenience for a
  user who wants every supported association.
- The project is licensed under the MIT License, allowing public
  redistribution.

## Options

| Method | Installation and updates | Reach | Fit for open-mpv | Decision |
| --- | --- | --- | --- | --- |
| RPM in GitHub Releases | One DNF URL; manual update | Fedora 44 x86-64 | Minimal maintenance for the current audience | Use now |
| Fedora RPM in Copr | DNF repository and automatic updates | Fedora | Same RPM with repository maintenance | Add when requested |
| Flatpak repository | Flatpak | Desktop Linux | Needs sandbox and media-path validation | Defer until cross-distribution demand |
| Standalone archive | Manual | Theoretical | Leaves dynamic-library compatibility to users | Debug builds only |
| AppImage | Usually manual | Theoretical | Poor fit for system codecs and graphics drivers | Do not prioritize |
| Build from source | Manual rebuild | Developers | Contributor workflow | Keep for contributors |

### Why GitHub Releases first

The current user base is the author and a small number of Fedora users. A
GitHub-hosted RPM preserves the exact Fedora library and media stack used for
development without requiring a package repository to be operated. DNF can
install an HTTPS RPM directly and resolve its dependencies from Fedora.

Every release uploads the same external asset name,
`open-mpv-fedora44-x86_64.rpm`. GitHub's
`releases/latest/download/<asset>` redirect therefore provides one install and
update URL while the RPM retains its real version internally. GitHub is not a
DNF repository: `dnf upgrade` cannot discover these releases, so the user must
re-run the install command after a release announcement.

### When to add Copr

Move the existing RPM to Copr when unattended discovery through `dnf upgrade`
would materially help the user base. Copr should consume the same tagged
source and spec rather than becoming a second packaging implementation.

### Why Flatpak needs a prototype

Flatpak broadens reach but changes filesystem and runtime boundaries. Prototype
a disposable manifest on real Wayland and run the requirements with particular
attention to folder access and monitoring, trash/restore and atomic saves,
glycin loaders, codecs, compatible hardware decoding and software fallback,
configuration location,
single-instance activation, cold start, PSS and installed size.

The result should use the narrowest permissions that preserve the product. If
folder navigation or file operations require broad host filesystem access, make
that trade-off explicit before publishing.

At the last review on 20 August 2026, Flathub's requirements made this
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

Use the semantic version from `Cargo.toml` in signed tags such as `v0.1.0`.
The tag, RPM spec and AppStream release must carry the same version. Release
notes cover user-visible changes, fixes, known issues and configuration
changes.

The update flow should be:

```text
version + release metadata -> signed source tag -> automatic Prepare release workflow
                                  -> Fedora 44 verification and RPM build
                                  -> reviewed GitHub draft and assets
                                  -> user re-runs the stable DNF URL
```

Pushing a signed version tag starts the Prepare release workflow. A maintainer
can also start it manually with an existing tag when a retry is needed. The
workflow rejects a lightweight, unsigned or GitHub-unverified tag, checks out
the verified tag's exact commit, runs the complete required checks, creates the
RPM from vendored locked Cargo sources, and verifies the package, lifecycle and
checksum. It then uses GitHub's repository-scoped token to create a draft
containing generated notes and the assets. It needs no maintainer token or
release secret. The maintainer completes the known-issue and configuration
sections, reviews the draft and explicitly publishes it. Build stable packages
only from tags; GitHub retains older releases for explicit downgrade.

Use no fixed calendar. Release meaningful improvements when ready; publish
security or data-safety fixes promptly with a plain impact statement.

## First-release gate

1. Add AppStream metadata, including a summary, description, screenshots,
   supported URLs, launchable desktop ID, releases, and developer name.
2. Make installation package-friendly: support staged installation into a
   supplied prefix, and separate installation from changing MIME defaults.
3. Add an RPM spec and build it in a clean Fedora environment.
4. Test fresh install, reinstall, uninstall, MIME registration and all
   media/file-operation paths. Complete human GNOME/Wayland launch testing.
   When a previous release exists, the workflow also tests upgrade and
   downgrade against its retained RPM.
5. Complete the author validation gate and explicitly approve publication.
6. Enable immutable GitHub Releases, push a signed release tag, wait for the
   Prepare release workflow, complete and publish its draft, then verify the
   stable GitHub URL through DNF.

## Decision checkpoints

After testing the first RPM and again before expanding distribution, answer:

- Does the package work on every Fedora release we claim to support?
- Do upgrades preserve config and file associations?
- Are startup time, PSS, and video hardware decoding unchanged?
- Can one maintainer reliably publish a fix using only a reviewed tag?
- Are users asking for automatic update discovery strongly enough to justify
  Copr maintenance?
- Are users outside Fedora actually asking for a package?

Add Copr only when automatic Fedora updates justify it. Run the Flatpak
prototype only when cross-distribution demand exists. Adopt Flatpak if it
passes the functional and performance checks without permissions that
undermine the product; otherwise keep Fedora as the honest supported target.

## References

- [GitHub: link to the latest release asset](https://docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases)
- [GitHub: manually run a workflow](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)
- [GitHub: verify signed tags](https://docs.github.com/en/authentication/managing-commit-signature-verification/about-commit-signature-verification)
- [DNF: install an RPM directly from a URL](https://dnf.readthedocs.io/en/stable/command_ref.html#install-command)
- [Fedora: publishing packages in Copr](https://docs.fedoraproject.org/en-US/quick-docs/publish-rpm-on-copr/)
- [GNOME: why Flatpak is recommended for GNOME apps](https://developer.gnome.org/documentation/introduction/flatpak.html)
- [Flatpak sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)
- [Flatpak repositories and updates](https://docs.flatpak.org/en/latest/repositories.html)
- [Flathub submission requirements](https://docs.flathub.org/docs/for-app-authors/requirements)
- [GLib application ID rules](https://docs.gtk.org/gio/type_func.Application.id_is_valid.html)
