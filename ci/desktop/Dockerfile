ARG BASE
FROM ${BASE}
ARG DESKTOP
ARG PROBE_ONLY=0
COPY ci/desktop/prepare-guest.sh /tmp/prepare-guest.sh
RUN PROBE_ONLY=${PROBE_ONLY} /tmp/prepare-guest.sh "${DESKTOP}"
WORKDIR /opt/open-mpv
COPY --chown=ci:ci Cargo.toml Cargo.lock build.rs ./
COPY --chown=ci:ci src/ src/
COPY --chown=ci:ci data/ data/
COPY --chown=ci:ci docs/CONFIGURATION.md docs/CONFIGURATION.md
COPY ci/desktop/build-guest.sh /tmp/build-guest.sh
RUN PROBE_ONLY=${PROBE_ONLY} /tmp/build-guest.sh
COPY ci/desktop/ ci/desktop/
