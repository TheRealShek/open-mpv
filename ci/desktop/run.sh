#!/usr/bin/env bash
# Run only on a disposable Docker/KVM runner. No desktop sockets are mounted.
set -euo pipefail
desktop=$1
artifacts=$2
shift 2
case "$desktop" in gnome|hyprland) ;; *) exit 2 ;; esac
case "$artifacts" in /*) ;; *) echo 'Artifact path must be absolute' >&2; exit 2 ;; esac
mkdir -p "$(dirname "$artifacts")"
mkdir "$artifacts"
image="open-mpv-vm-runner:$desktop-$$"
container="open-mpv-vm-$desktop-$$"
label="io.github.therealshek.openmpv.ci-run=$container"

cleanup() {
  code=$?
  trap - EXIT
  if docker container inspect "$container" >/dev/null 2>&1; then
    docker stop --time 15 "$container" >> "$artifacts/cleanup.log" 2>&1 || code=1
    if docker container inspect "$container" >/dev/null 2>&1; then
      docker rm -f "$container" >> "$artifacts/cleanup.log" 2>&1 || code=1
    fi
  fi
  if leftovers=$(docker ps -aq --filter "label=$label" 2>> "$artifacts/cleanup.log"); then
    for resource in $leftovers; do
      docker rm -f "$resource" >> "$artifacts/cleanup.log" 2>&1 || code=1
    done
  else
    code=1
  fi
  if leftovers=$(docker image ls -q --filter "label=$label" 2>> "$artifacts/cleanup.log" | sort -u); then
    for resource in $leftovers; do
      docker image rm "$resource" >> "$artifacts/cleanup.log" 2>&1 || code=1
    done
  else
    code=1
  fi
  status=failed
  if [ "$code" = 0 ]; then status=passed; fi
  printf '{"status":"%s","exit_code":%s}\n' "$status" "$code" > "$artifacts/bootstrap-result.json"
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

timeout --signal=TERM --kill-after=30s 15m \
  docker build --progress=plain --label "$label" -f ci/desktop/runner.Dockerfile -t "$image" . \
  > "$artifacts/runner-build.log" 2>&1
docker image inspect "$image" > "$artifacts/runner-image.json"
docker run --rm --init --name "$container" --device /dev/kvm \
  --env "OPEN_MPV_CI_RUN_ID=$container" \
  --mount type=bind,src=/var/run/docker.sock,dst=/var/run/docker.sock \
  --mount "type=bind,src=$PWD,dst=$PWD" --workdir "$PWD" \
  --mount "type=bind,src=$artifacts,dst=$artifacts" \
  "$image" python3 ci/desktop/host.py "$desktop" --artifacts "$artifacts/guest" "$@" \
  > "$artifacts/runner.log" 2>&1
