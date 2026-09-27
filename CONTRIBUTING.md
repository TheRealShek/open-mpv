# Contributing to open-mpv

open-mpv is built and tested for Fedora Workstation, GNOME and Wayland. Before
changing behavior, read the [product context](CONTEXT.md) and the relevant
[requirement](docs/REQUIREMENTS.md). Packaging and release work follows the
[distribution guide](docs/DISTRIBUTION.md).

## Build from source

Install the development dependencies:

```sh
sudo dnf install ImageMagick cargo desktop-file-utils gcc git glycin-devel \
  glycin-loaders gstreamer1-devel gstreamer1-plugin-gtk4 \
  gstreamer1-plugins-base gstreamer1-plugins-good gtk4-devel rust wayland-devel xdg-utils
```

Clone and run the project:

```sh
git clone https://github.com/TheRealShek/open-mpv.git
cd open-mpv
cargo run -- <file-or-folder>
```

To switch an existing source installation to the packaged release, follow the
[source-to-RPM migration guide](docs/DISTRIBUTION.md#migrate-a-source-installation-to-rpm).
Source scripts refuse package-owned destinations; use DNF for RPM installations.

For experimental Arch/Omarchy packaging, follow the
[native Arch package guide](docs/DISTRIBUTION.md#arch-linux-and-omarchy).
It builds a pinned release rather than local source changes. Fedora remains
the reference for full platform verification.

On Arch, once the package guide's build and runtime dependencies are installed,
use `cargo run --locked -- <file-or-folder>` from this checkout to test local
changes. Run the Cargo checks below against the checkout as well: `makepkg`
tests the pinned release with its packaging patch, not your working tree.
Validate recipe changes with `shellcheck packaging/arch/PKGBUILD` and
`bash -n packaging/arch/PKGBUILD`, then rebuild and test the package lifecycle
as described in the distribution guide. Fedora CI remains required.

## Check a change

Start with the smallest test that covers the behavior you changed. Before
opening a pull request, run the relevant repository checks:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Changes to scripts, packaging or application metadata also need the relevant
ShellCheck, desktop-file, AppStream and RPM validation. See the
[distribution guide](docs/DISTRIBUTION.md) before changing packaging or release
behavior.

Some behavior, including keyboard, pointer, clipboard and visual interaction,
must also be tested in a real GNOME Wayland session.
