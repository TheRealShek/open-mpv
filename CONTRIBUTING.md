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
  gstreamer1-plugins-base gstreamer1-plugins-good gtk4-devel rust xdg-utils
```

Clone and run the project:

```sh
git clone https://github.com/TheRealShek/open-mpv.git
cd open-mpv
cargo run -- <file-or-folder>
```

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
