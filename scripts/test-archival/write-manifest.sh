#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

: "${RUN_DIR:?}"
: "${TIER:?}"

started="${STARTED_AT:-}"
finished="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
rustc_v="$(rustc -vV | tr '\n' ' ')"
nextest_v="$(cargo nextest --version 2>/dev/null | tr '\n' ' ' || echo "nextest-not-installed")"
llvm_cov_v="$(cargo llvm-cov --version 2>/dev/null | tr '\n' ' ' || echo "llvm-cov-not-installed")"

base_json="$(jq -n \
  --arg run_id "$(basename "$RUN_DIR")" \
  --arg tier "$TIER" \
  --arg started "$started" \
  --arg finished "$finished" \
  --arg git_commit "$(git rev-parse HEAD)" \
  --argjson git_dirty "$(git status --porcelain | grep -q . && echo true || echo false)" \
  --arg rustc "$rustc_v" \
  --arg host "$(uname -srmo)" \
  --arg nextest "$nextest_v" \
  --arg llvm_cov "$llvm_cov_v" \
  --argjson artifact_layout_version 1 \
  '{
    run_id: $run_id,
    tier: $tier,
    started_at: $started,
    finished_at: $finished,
    git_commit: $git_commit,
    git_dirty: $git_dirty,
    rustc_version: $rustc,
    host_os: $host,
    nextest_version: $nextest,
    llvm_cov_version: $llvm_cov,
    artifact_layout_version: $artifact_layout_version
  }')"

if [[ -n "${EXTRA_JSON:-}" && -f "$EXTRA_JSON" ]]; then
  jq -s '.[0] * .[1]' <(echo "$base_json") "$EXTRA_JSON" >"$RUN_DIR/manifest.json"
else
  echo "$base_json" >"$RUN_DIR/manifest.json"
fi
