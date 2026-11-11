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

The build embeds the viewer's SVG icons using `glib-compile-resources`, supplied
by GLib with the development stack above. Viewer icons come from Lucide. See the
[icon source and adaptation notes](data/icons/README.md) before adding or updating
them. Use the `open-mpv-` icon names in widgets and state updates so desktop themes
do not replace them.
GTK's symbolic parser requires paint classes on each shape rather than inherited
group styles. Outline shapes use `transparent-fill foreground-stroke`; solid
shapes use `foreground-fill`. Keep the standard SVG paint attributes as well so
the files also render in ordinary SVG tools.

After changing icons, run the offscreen rendering check in a Wayland session:

```sh
G_DEBUG=fatal-criticals cargo test --locked window::assembly::tests::bundled_icons_render_monochrome_across_themes_and_scales -- --ignored --exact
```

It checks embedded icon resolution and monochrome rendering with Adwaita, Yaru
and the hicolor fallback at normal, fractional and double icon sizes. This does
not replace visual testing of the full Viewer at actual compositor scales.

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

The SVG ordering and high-precision cache regressions use unmapped GTK windows.
Run them separately in a Wayland session:

```sh
cargo test --locked window::decode_tests::svg_zoom_requests_are_bounded_and_superseded_results_never_display -- --ignored --exact
cargo test --locked window::decode_tests::high_precision_navigation_evicts_neighbors_and_settles_pss -- --ignored --exact --nocapture
```

The memory check needs ImageMagick, generates temporary 12 MP RGBA16 TIFFs and
reports Linux process-tree PSS over repeated navigation and after cleanup.
Texture downloads fault in the CPU mappings without presenting a window.
These checks do not establish displayed behavior or GPU memory use.

## Disposable desktop CI

CI keeps the fast Fedora checks and runs two independent desktop jobs on
GitHub's Ubuntu 24.04 runners: Fedora 44 with GNOME Shell/Wayland, and rolling
Arch with Hyprland. Neither job uses a developer's desktop, home, configuration,
trash, display sockets or D-Bus. A Fedora runner-tools container builds
distribution root filesystems with Docker, then boots disposable QEMU/KVM
guests with a virtual GPU. Virgl uses
software rendering on an isolated Xvfb display on the runner. The application
itself uses native Wayland inside the guest.

Before testing the application, a displayed GTK probe must respond to a virtual
keyboard key and pointer double-click. QEMU supplies both through virtual input
devices. Screenshots must show the expected red-to-green change and fullscreen
state. A compositor IPC call only observes window state; it never substitutes
for keyboard or pointer input.

The shared Viewer scenario checks displayed image navigation, pointer
fullscreen, keyboard exit from fullscreen, Trash and exact-byte Undo, and
single-instance forwarding. GNOME also checks the Shell overview shortcut and
focus return. Hyprland checks a compositor-owned binding and idle-inhibitor
release/resumption around video pause. All input uses the same QEMU route.

The guest also reuses the Rust suite. Every ignored test is discovered and run
in its own process, with `--ignored --exact` and a 180-second deadline. The
harness rejects zero-test success. Generated fixtures provide three colored
PNGs, twelve stress images, a broken image, and a fixed VP8 video. File hashes
and package versions accompany results. Ordinary Rust tests run as the guest
user with a private session bus and trash. Glycin's sandbox remains enabled;
the test does not disable bubblewrap, relax the runner's container security
policy, or forward host devices into the guest.

Run the harness unit tests without a desktop:

```sh
python3 -m unittest discover -s ci/desktop/tests -v
shellcheck ci/desktop/*.sh
bash -n ci/desktop/*.sh
```

To reproduce a job, use a **disposable Linux runner** with Docker and KVM.
The wrapper builds the VM tools container from Fedora's signed packages;
it does not install host packages. The runner needs enough space for a distribution root filesystem
and a 12 GiB sparse guest disk. Do not run guest provisioning scripts on your
workstation. From this checkout:

```sh
ci/desktop/run.sh gnome "$PWD/desktop-results/gnome"
ci/desktop/run.sh hyprland "$PWD/desktop-results/hyprland"
```

Use a new artifact directory for each run. Add `--probe` to prove boot,
compositor readiness and routed input without compiling Rust. The CI workflow's
`desktop_probe` dispatch input runs this same smaller proof. A successful probe
is explicitly marked `probe_only`; it is not an application-suite pass.

Artifacts are retained for 14 days, including failed runs: build, serial,
compositor and per-test logs, screenshots, progressive and final JSON results,
fixture hashes, guest package/kernel details, image identity and QEMU version
and command. Bootstrap failures retain a separate result and runner build log.
Setup errors, missing results, timeouts and test failures fail the
job. Process groups are stopped on completion or interruption, and the guest
disk and extracted root are removed. The matrix does not cancel one desktop
when the other fails.

These jobs establish only their recorded scenarios. GNOME runs the actual
Shell in a native logind/Wayland login, not bare Mutter. It does not run GDM or
the complete GNOME session manager and portal stack. This is GNOME Shell
integration coverage, not full GNOME Workstation acceptance. Software-rendered
virtual hardware does not establish physical GPU decoding, real-device
performance, fractional-scale appearance, packaging lifecycle, or all clipboard
and accessibility interactions. Orca speech is excluded. Human desktop and
hardware verification remain required by FR-9.4.

The known installed-loader Radiance coverage failure remains a baseline
failure until the format allowlist is fixed separately. It is not filtered,
marked successful, or silently skipped by this harness. Inspect the per-test
logs and final results instead of interpreting artifact upload as test success.
