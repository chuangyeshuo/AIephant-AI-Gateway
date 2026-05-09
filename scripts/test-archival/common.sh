#!/usr/bin/env bash
# Sourced by run-gate.sh / run-nightly.sh; do not run directly
set -euo pipefail

common_init() {
  export REPO_ROOT
  REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  cd "$REPO_ROOT"

  local short_sha
  short_sha="$(git rev-parse --short HEAD)"
  export RUN_ID="${RUN_ID_OVERRIDE:-$(date -u +%Y%m%dT%H%M%SZ)-${short_sha}}"
  export RUN_DIR="$REPO_ROOT/test-artifacts/runs/$RUN_ID"
  mkdir -p "$RUN_DIR/logs" "$RUN_DIR/gate" "$RUN_DIR/nightly/coverage" "$RUN_DIR/nightly/traces"
}

junit_src_for_profile() {
  local profile="$1"
  echo "$REPO_ROOT/target/nextest/${profile}/junit.xml"
}

copy_junit() {
  local profile="$1"
  local dest="$2"
  local src
  src="$(junit_src_for_profile "$profile")"
  if [[ -f "$src" ]]; then
    cp "$src" "$dest"
  else
    echo "WARN: missing junit at $src" >&2
  fi
}
