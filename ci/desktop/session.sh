#!/usr/bin/env bash
set -euo pipefail
# A fresh login, bus and home for every boot. No host sockets are forwarded.
export HOME=/home/ci/test-home
export XDG_CONFIG_HOME=$HOME/.config XDG_DATA_HOME=$HOME/.local/share
export XDG_CACHE_HOME=$HOME/.cache XDG_STATE_HOME=$HOME/.local/state
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME"
export XDG_SESSION_TYPE=wayland GDK_BACKEND=wayland
export RUST_BACKTRACE=1 CARGO_TERM_COLOR=never
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
exec dbus-run-session -- python3 ci/desktop/session.py "$(cat /etc/open-mpv-ci-desktop)"
