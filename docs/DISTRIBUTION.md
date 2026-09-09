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

## Migrate a source installation to RPM

A source build in `~/.local` can shadow the RPM through both `PATH` and the
per-user desktop launcher. RPM installation never deletes home-directory
files. Identify the installation before removing anything:

```sh
type -a open-mpv
command -v open-mpv
rg '^Exec=' ~/.local/share/applications/io.github.TheRealShek.OpenMpv.desktop
```

A missing per-user launcher is normal. Also inspect
`${XDG_DATA_HOME:-$HOME/.local/share}/applications` if you use a custom XDG data
location, and `dev.thakur.OpenMpv.desktop` for pre-release source installations.
An absolute `Exec=` path identifies the old source prefix. Use the prefix you
actually installed, which may differ from the current command found on PATH.
Do not delete unknown launchers or binaries just because their names match.

1. Close open-mpv completely so single-instance activation cannot send requests
   to an already-running source build. From the current source checkout, remove
   the known previous source installation with its correct prefix:

   ```sh
   ./uninstall.sh --prefix "$HOME/.local"
   ```

   For a custom installation, substitute its actual prefix. This removes only
   the known installed application files and refreshes desktop caches. It
   preserves configuration, media and file-association preferences. Source
   scripts preflight all affected destinations and refuse RPM-owned paths with
   a DNF message before modifying any installation files. If ownership conflicts,
   resolve the package through DNF; do not force the source script over it.
2. Install the RPM using the existing stable URL:

   ```sh
   sudo dnf install https://github.com/TheRealShek/open-mpv/releases/latest/download/open-mpv-fedora44-x86_64.rpm
   ```

3. Open a fresh terminal (or run `rehash` in zsh), then verify the executable:

   ```sh
   command -v open-mpv
   rpm -qf "$(command -v open-mpv)"
   rpm -V open-mpv
   rg '^Exec=' /usr/share/applications/io.github.TheRealShek.OpenMpv.desktop
   ```

   The command should resolve to `/usr/bin/open-mpv`, owned by the `open-mpv`
   RPM, and the system launcher should use `/usr/bin`. Confirm that no old
   per-user launcher with the same application ID remains. If GNOME still shows
   an old launcher, sign out and back in after removing the known source copy.
4. Launch from GNOME and inspect `readlink /proc/$(pgrep -n -x open-mpv)/exe`;
   it should show `/usr/bin/open-mpv`. Close the app, launch from the terminal
   with a local media file, and repeat that check. Confirm your existing
   configuration and chosen default associations still apply.

Legitimate per-user and custom-prefix source installations remain supported.
Source scripts require working RPM tooling to check ownership. Staged packaging
with `install.sh --no-build --prefix /usr --destdir <buildroot>` checks the
actual staged destinations, including symlink targets; it does not reject a
safe buildroot merely because the live `/usr` installation is package-owned.
Repeated uninstallation remains safe when unowned files are already absent.

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
version + release metadata -> signed source tag -> automatic Package signed release workflow
                                  -> Fedora 44 verification and RPM build
                                  -> reviewed GitHub draft and assets
                                  -> user re-runs the stable DNF URL
```

Pushing a signed version tag starts the Package signed release workflow. A maintainer
can also start it manually with an existing tag when a retry is needed. The
workflow rejects a lightweight, unsigned or GitHub-unverified tag, checks out
the verified tag's exact commit, runs the complete required checks, creates the
RPM from vendored locked Cargo sources, and verifies the package, lifecycle and
checksum. It then uses GitHub's repository-scoped token to create or resume a draft
containing generated notes and the assets. It needs no maintainer token or
release secret. The maintainer completes the known-issue and configuration
sections, reviews the draft and explicitly publishes it. Build stable packages
only from tags; GitHub retains older releases for explicit downgrade.

Use no fixed calendar. Release meaningful improvements when ready; publish
security or data-safety fixes promptly with a plain impact statement.

## Prepare a release in the browser

After merging the release automation into `main` and completing the one-time
signing setup below:

Use **Prepare release** (`prepare-release.yml`) for a new version. **Package
signed release** (`release.yml`) is only for an existing signed tag or packaging
retry. Typing a new version into the packaging workflow does not create its tag.
If the Prepare release form shows only a `tag` field, `main` still has the old
workflow. Make sure the automation PR actually targeted and merged into `main`;
merging stacked PRs into a previously merged feature branch does not update main.

Then:

1. Open **Actions → Prepare release → Run workflow** on `main`. Enter a new
   stable version such as `0.1.3` and a short single-line description. Leave
   `pr` empty. The workflow links a preparation PR that updates Cargo metadata,
   the RPM changelog and AppStream together without updating dependencies.
2. Open the PR. If GitHub displays **Approve workflows to run**, approve it;
   repository-token-created PRs require this step before their CI can run.
   Review the metadata and successful required checks, then merge normally.
   Automation never approves or merges its own PR or bypasses main protection.
3. The merged PR starts a signing run. Review its PR and exact merge commit,
   then approve the `release-signing` environment. The job validates the merged
   metadata against the request and signs that commit, even if `main` advances.
   It refuses unrelated PR changes, changed metadata or a conflicting tag.
4. Follow the explicitly called **Package signed release** job to its draft,
   source manifest and RPM SHA-256 summary. Download the draft assets, validate
   the exact RPM, and follow the immutable publication checklist below.
   Publication is always manual.

Only one preparation may remain active, including a merged preparation waiting
for publication. Repeating its version links the existing PR without resetting
its branch or edited text. Finish that release before requesting another one;
close an unmerged PR to abandon it. An orphaned preparation branch is never
reset automatically: inspect it and open its PR manually. The request file
`.github/release-request.json` is reviewed release metadata, not a secret.

If signing is denied or a run fails after merge, rerun **Prepare release** on
`main` with only the merged `pr` number. This revalidates that exact PR and asks
for signing approval again. An existing verified tag at that commit is reused;
a conflicting tag is never overwritten. Packaging retries use the shared path
below. Repository-token tag pushes do not start tag workflows, so the browser
flow explicitly calls packaging. Locally signed tags remain supported through
**Package signed release**.

### One-time signing setup and recovery

On 9 September 2026, Actions PR creation was enabled and `release-signing` was
created with the owner as required reviewer, self-approval allowed, and a
custom deployment policy allowing only the `main` branch. Verify these settings
before use. Keep default workflow permissions read-only and main's required PR
and CI rules enabled. GitHub groups its Actions PR creation and approval setting;
although enabled, these workflows never approve PRs.

Provision a **dedicated release-only OpenPGP signing key**, separate from the
owner's everyday key, with a signing-capable primary key and an email verified
on the owner's GitHub account. Register its armored public key under GitHub
**Settings → SSH and GPG keys** so GitHub can verify its tags. Record the full
40-character uppercase fingerprint independently. Export only that dedicated
private key into the `release-signing` environment secret `RELEASE_SIGNING_KEY`.
Set environment variables `RELEASE_SIGNING_FINGERPRINT` to the recorded primary
fingerprint and `RELEASE_SIGNING_EMAIL` to the registered email.

The automation key must support unattended signing within the approved job
(no interactive passphrase prompt). Protect its private export in the GitHub
environment, retain an offline recovery/revocation copy securely, and never put
it in repository files, workflow inputs or logs. Signing imports it into a
private temporary GnuPG home and removes that home on exit. Only the signing
step receives the key; preparation PRs and build/test jobs do not receive it.
The job pins and verifies the fingerprint before signing, and GitHub must
verify the pushed tag before packaging proceeds.

For rotation, create and register a new dedicated key, then replace the secret,
fingerprint and email together while no signing job is active, and update the
committed public key in the same reviewed rotation. For compromise,
disable signing runs, revoke/remove the compromised key in GitHub, and provision
a replacement. Investigate any affected tags; never move a conflicting or
published tag. Resume only reviewed commits, or prepare a new version when a
bad tag has consumed the requested version. Existing published assets stay fixed.

A dedicated one-year key was generated on 9 September 2026 and installed in the
protected environment with its fingerprint and email. Its public half is
[release-signing.asc](../.github/release-signing.asc); the private key is never
committed. The local recovery keyring and revocation certificate are stored
under `~/.local/share/open-mpv-release-signing` with owner-only access. Keep a
secure offline backup and rotate the automation key before it expires.

The owner must register the complete public file at
[GitHub → New GPG key](https://github.com/settings/gpg/new), titled
`open-mpv release signing`. This is a one-time browser action; routine releases
need no terminal. The current maintainer CLI token cannot perform this
account-level registration because it lacks `admin:gpg_key`. Preparation checks
GitHub registration and the verified signing email before creating a PR.

A complete rehearsal in an isolated repository remains required before using
this browser flow for a production release. The
rehearsal must cover PR CI approval, merge and owner signing approval, moved
main, denied approval, tag conflicts, failed-upload recovery and preserved draft
notes. Do not use a dummy production release for these tests.

## Retrying release preparation

Run **Actions → Package signed release → Run workflow** with the existing signed tag.
Validation runs before Fedora setup and rejects invalid versions, unsigned or
unverified tags, mismatched source versions and published releases. Preparation
for the same tag is serialized across automatic and manual requests. GitHub
may replace a pending request with a newer pending request; it does not cancel
an active preparation. Never move an existing tag to resolve a conflict.

Retries retain the draft's title and maintainer-edited notes. Completed assets
are downloaded and checked against `release-source.json` (tag, signed tag object
and exact commit) and `SHA256SUMS`, then the package checks run again on those
same bytes. The manifest is uploaded first. A draft with no assets, or only a
matching manifest, can resume building. No upload overwrites an existing asset.

A partial upload, missing provenance for an older draft, mismatched checksum or
conflicting bytes stops preparation. Inspect the failed run and draft before
recovery. While it is still a draft, either restore the original matching asset
set from the exact failed run or explicitly remove all three preparation assets
(`release-source.json`, RPM, `SHA256SUMS`) and retry. Retain edited notes; do not
delete the release. Stop other preparation runs before manual asset recovery.
Rebuilding or replacing assets requires renewed author validation even at the
same commit. Never recover by deleting or changing a published release.

Upgrade/downgrade testing examines all published stable releases, downloads
matching Fedora 44 x86-64 RPMs, and chooses the highest package epoch/version/
release strictly below the candidate using native RPM comparison. Equal,
newer and incompatible packages are excluded. The run explicitly reports when
no predecessor exists and skips only that upgrade/downgrade portion.

## Immutable publication checklist

Repository release immutability was enabled and verified on 9 September 2026.
Before each publication, confirm **Settings → General → Releases → Enable
release immutability** is still enabled. This protects future publications;
existing published versions remain untouched. Never delete and republish a
published version to correct it: use a new version.

1. Keep the release as a draft while attaching the Fedora RPM and `SHA256SUMS`.
   Download both draft assets into a fresh directory and run `sha256sum -c
   SHA256SUMS` there. Record the tag, exact source commit, workflow run and RPM
   SHA-256 in the validation record.
2. Validate that exact downloaded RPM in GNOME/Wayland, including launch and
   the applicable packaging checks. The author must test that artifact or
   explicitly approve publication without personal testing, recording the
   remaining risk. A rebuild or replacement invalidates previous artifact
   testing, even when the version and source commit are unchanged; repeat the
   validation gate for the replacement bytes.
3. Complete the draft notes, including known issues and configuration changes,
   and publish manually only after approving the exact attached assets. After
   publication, the tag and assets are immutable; the title and notes remain
   editable. Corrections to a binary require a new release version.
4. For the next normally authorized publication, confirm the release page
   shows **Immutable** and verify its attestation and local assets with a
   GitHub CLI version that supports release verification:

   ```sh
   # Set this to the version just published; run beside its downloaded assets.
   tag=v0.1.3
   gh release verify "$tag" --repo TheRealShek/open-mpv
   gh release verify-asset "$tag" open-mpv-fedora44-x86_64.rpm --repo TheRealShek/open-mpv
   gh release verify-asset "$tag" SHA256SUMS --repo TheRealShek/open-mpv
   ```

5. When publishing the new latest stable release, download the stable URL and
   compare its bytes with the approved RPM before using the documented DNF
   install command:

   ```sh
   curl --fail --location --output latest.rpm \
     https://github.com/TheRealShek/open-mpv/releases/latest/download/open-mpv-fedora44-x86_64.rpm
   cmp open-mpv-fedora44-x86_64.rpm latest.rpm
   ```

Publication verification remains pending until the next authorized release.
Do not publish a dummy production release for this check. Retain the successful
verification output with that release's validation record.

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
   Package signed release workflow, complete and publish its draft, then verify the
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

- [GitHub: immutable releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases)
- [GitHub: enable release immutability](https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/establish-provenance-and-integrity/prevent-release-changes)
- [GitHub CLI: verify release assets](https://cli.github.com/manual/gh_release_verify-asset)

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

## Diagnostic builds

Normal release binaries and RPMs remain stripped; the RPM does not currently
publish a debuginfo package. A backtrace from a stripped release may lack useful
application frames and source locations. Do not substitute symbols from a
separate build when analyzing an existing core.

For reproduction, use the source revision matching the affected release and
build the local `diagnostic` Cargo profile. It inherits release optimization,
retains full debug information, and disables stripping. Keep that exact binary
and source revision with any captured core or panic trace. Run it directly;
do not pass it through the installer or RPM stripping step. This is a local
diagnostic workflow, not a new distributed artifact or supported package.
[The troubleshooting guide](TROUBLESHOOTING.md#unexpected-crashes) owns the
commands and distinguishes panic backtraces from ordinary returned errors.
