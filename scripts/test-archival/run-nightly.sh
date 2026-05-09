#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

common_init
export STARTED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
export NEXTEST_PROFILE=ci-nightly

main_log="$RUN_DIR/logs/nightly.log"
trace_log="$RUN_DIR/nightly/traces/rust-log.txt"
mkdir -p "$(dirname "$trace_log")"

# Structured / semi-structured trace: default info; export RUST_LOG=debug for more detail
export RUST_LOG="${RUST_LOG:-info}"

exec > >(tee >(bash "$SCRIPT_DIR/redact-log.sh" >"$trace_log") | tee -a "$main_log") 2>&1

declare -a cmds=()

run_nextest() {
  echo "==> $1"
  shift
  cmds+=("$*")
  "$@"
}

# 1) ai-gateway: integration tests (matches current CI)
run_nextest "ai-gateway integration (nextest)" \
  cargo nextest run --profile ci-nightly \
  -p ai-gateway --features "external integration" --tests
copy_junit ci-nightly "$RUN_DIR/nightly/junit-ai-gateway-integration.xml"

# 2) Coverage: primary scope ai-gateway (merging other crates deferred)
if [[ "${SKIP_LLVM_COV:-0}" == "1" ]]; then
  echo "SKIP_LLVM_COV=1 set, skipping llvm-cov"
else
  echo "==> llvm-cov (ai-gateway integration)"
  cmds+=("cargo llvm-cov nextest run --profile ci-nightly -p ai-gateway --features \"external integration\" --tests --lcov --output-path \"$RUN_DIR/nightly/coverage/lcov.info\" --html --output-dir \"$RUN_DIR/nightly/coverage/html\"")
  cargo llvm-cov nextest run --profile ci-nightly \
    -p ai-gateway --features "external integration" --tests \
    --lcov --output-path "$RUN_DIR/nightly/coverage/lcov.info" \
    --html --output-dir "$RUN_DIR/nightly/coverage/html"
fi

tmp_cmds="$(mktemp)"
jq -n \
  --argjson c "$(printf '%s\n' "${cmds[@]}" | jq -R . | jq -s .)" \
  --arg cov "$( [[ "${SKIP_LLVM_COV:-0}" == "1" ]] && echo skipped || echo written )" \
  '{commands: $c, coverage: { nightly_primary_package: "ai-gateway", lcov: $cov }}' >"$tmp_cmds"

export TIER=nightly
export EXTRA_JSON="$tmp_cmds"
bash "$SCRIPT_DIR/write-manifest.sh"
rm -f "$tmp_cmds"

echo "Nightly artifacts under: $RUN_DIR"
