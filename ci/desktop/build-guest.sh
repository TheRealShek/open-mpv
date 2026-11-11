#!/usr/bin/env bash
set -euo pipefail
if [ "${PROBE_ONLY:-0}" != 1 ]; then
runuser -u ci -- env CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  bash -euc 'cargo build --locked; cargo test --locked --no-run --message-format=json > /tmp/test-build.json'
mkdir -p bin
cp target/debug/open-mpv bin/open-mpv
python3 - <<'PY'
import json
import shutil
from pathlib import Path

artifacts = [json.loads(line) for line in Path('/tmp/test-build.json').read_text().splitlines()]
tests = [a['executable'] for a in artifacts
         if a.get('reason') == 'compiler-artifact' and a.get('profile', {}).get('test')
         and a.get('executable')]
assert len(tests) == 1, tests
shutil.copy2(tests[0], 'bin/tests')
PY
rm -rf target /home/ci/.cargo /tmp/test-build.json
fi
