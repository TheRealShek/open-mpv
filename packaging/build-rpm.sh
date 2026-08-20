#!/bin/sh
# Build the Fedora 44 x86-64 release RPM from the current working tree.
set -eu

SRC_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
SPEC_FILE="${SRC_DIR}/packaging/fedora/open-mpv.spec"
METAINFO_FILE="${SRC_DIR}/data/io.github.TheRealShek.OpenMpv.metainfo.xml"
OUTPUT_DIR="${SRC_DIR}/dist"
EXPECTED_TAG=${1:-}

if [ "$(rpm -E '%fedora')" != 44 ]; then
    echo "RPM releases must be built on Fedora 44" >&2
    exit 1
fi
if [ "$(uname -m)" != x86_64 ]; then
    echo "RPM releases currently support only x86-64" >&2
    exit 1
fi

VERSION=$(sed -n '/^\[package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' \
    "${SRC_DIR}/Cargo.toml")
SPEC_VERSION=$(rpmspec -q --qf '%{VERSION}\n' "${SPEC_FILE}" | sort -u)
METAINFO_VERSION=$(sed -n 's/.*<release version="\([^"]*\)".*/\1/p' \
    "${METAINFO_FILE}" | head -n 1)

if [ -z "$VERSION" ] || [ "$VERSION" != "$SPEC_VERSION" ] \
    || [ "$VERSION" != "$METAINFO_VERSION" ]; then
    echo "Cargo.toml, AppStream metadata and the RPM spec must use one version" >&2
    exit 1
fi
if [ -n "$EXPECTED_TAG" ] && [ "$EXPECTED_TAG" != "v${VERSION}" ]; then
    echo "release tag ${EXPECTED_TAG} does not match version v${VERSION}" >&2
    exit 1
fi

BUILD_DIR=$(mktemp -d /tmp/open-mpv-rpm.XXXXXX)
trap 'rm -rf -- "$BUILD_DIR"' EXIT HUP INT TERM
SOURCE_ROOT="${BUILD_DIR}/open-mpv-${VERSION}"
RPM_ROOT="${BUILD_DIR}/rpmbuild"

mkdir -p "${SOURCE_ROOT}" "${SOURCE_ROOT}/.cargo" \
    "${RPM_ROOT}/BUILD" "${RPM_ROOT}/BUILDROOT" "${RPM_ROOT}/RPMS" \
    "${RPM_ROOT}/SOURCES" "${RPM_ROOT}/SPECS" "${RPM_ROOT}/SRPMS"

tar -C "${SRC_DIR}" --exclude=.git --exclude=dist --exclude=target -cf - . \
    | tar -C "${SOURCE_ROOT}" -xf -
(
    cd "${SOURCE_ROOT}"
    cargo vendor --quiet --locked --versioned-dirs vendor > .cargo/config.toml
)
tar -C "${BUILD_DIR}" -czf \
    "${RPM_ROOT}/SOURCES/open-mpv-${VERSION}.tar.gz" \
    "open-mpv-${VERSION}"

if [ "${OPEN_MPV_RPMBUILD_NODEPS:-0}" = 1 ]; then
    # Useful in an unprivileged development environment where a declared
    # BuildRequires tool is supplied through PATH instead of the RPM database.
    rpmbuild -ba "${SPEC_FILE}" --nodeps --define "_topdir ${RPM_ROOT}"
else
    rpmbuild -ba "${SPEC_FILE}" --define "_topdir ${RPM_ROOT}"
fi

RPM_FILE=$(find "${RPM_ROOT}/RPMS/x86_64" -maxdepth 1 -type f \
    -name 'open-mpv-*.x86_64.rpm' -print -quit)
if [ -z "$RPM_FILE" ]; then
    echo "rpmbuild did not produce the expected x86-64 package" >&2
    exit 1
fi

mkdir -p "${OUTPUT_DIR}"
install -m 644 "$RPM_FILE" "${OUTPUT_DIR}/open-mpv-fedora44-x86_64.rpm"
(
    cd "${OUTPUT_DIR}"
    sha256sum open-mpv-fedora44-x86_64.rpm > SHA256SUMS
)

echo "Built ${OUTPUT_DIR}/open-mpv-fedora44-x86_64.rpm"
