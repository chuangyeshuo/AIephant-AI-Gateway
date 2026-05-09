#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

common_init
export STARTED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
export NEXTEST_PROFILE=ci-gate

main_log="$RUN_DIR/logs/gate.log"
exec > >(tee -a "$main_log") 2>&1

mkdir -p "$RUN_DIR/gate"
libtest_jsonl="$RUN_DIR/gate/libtest-events.jsonl"
: >"$libtest_jsonl"

declare -a cmds=()

append_junit_captures_to_log() {
  local dest="$1"
  local label="$2"
  if [[ ! -f "$dest" ]]; then
    return 0
  fi
  python3 "$SCRIPT_DIR/gate_junit_captures.py" append-log "$dest" "$label"
}

enrich_junit_from_libtest() {
  local dest="$1"
  local label="$2"
  if [[ ! -f "$dest" ]]; then
    return 0
  fi
  python3 "$SCRIPT_DIR/enrich_junit_libtest.py" "$libtest_jsonl" "$dest" "$label"
}

run_nextest() {
  local desc="$1"
  shift
  echo "==> $desc: $* (NEXTEST_EXPERIMENTAL_LIBTEST_JSON + libtest-json-plus -> gate/libtest-events.jsonl)"
  cmds+=("$*")
  export NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1
  # shellcheck disable=SC2086
  "$@" --message-format libtest-json-plus 2>&1 \
    | python3 "$SCRIPT_DIR/gate_nextest_human_json_mux.py" \
      --jsonl "$libtest_jsonl" \
      --segment "$desc"
}

# 1) Small crates without conflicting features (excludes mock-server: it only enables testing on
#    ai-gateway, cannot pick external/internal for alephant-llm-kv-cache, and has no lib tests)
run_nextest "small workspace libs" \
  cargo nextest run --profile ci-gate --lib \
  -p telemetry -p weighted-balance -p dynamic-router -p latency-router
copy_junit ci-gate "$RUN_DIR/gate/junit-small-crates.xml"
enrich_junit_from_libtest "$RUN_DIR/gate/junit-small-crates.xml" "small workspace libs"
append_junit_captures_to_log "$RUN_DIR/gate/junit-small-crates.xml" "small workspace libs"

# 2) KV cache: external / internal as separate runs (mutually exclusive features)
run_nextest "alephant-llm-kv-cache external" \
  cargo nextest run --profile ci-gate --lib -p alephant-llm-kv-cache --features external
copy_junit ci-gate "$RUN_DIR/gate/junit-alephant-llm-kv-cache-external.xml"
enrich_junit_from_libtest "$RUN_DIR/gate/junit-alephant-llm-kv-cache-external.xml" \
  "alephant-llm-kv-cache external"
append_junit_captures_to_log "$RUN_DIR/gate/junit-alephant-llm-kv-cache-external.xml" \
  "alephant-llm-kv-cache external"

run_nextest "alephant-llm-kv-cache internal" \
  cargo nextest run --profile ci-gate --lib -p alephant-llm-kv-cache --features internal
copy_junit ci-gate "$RUN_DIR/gate/junit-alephant-llm-kv-cache-internal.xml"
enrich_junit_from_libtest "$RUN_DIR/gate/junit-alephant-llm-kv-cache-internal.xml" \
  "alephant-llm-kv-cache internal"
append_junit_captures_to_log "$RUN_DIR/gate/junit-alephant-llm-kv-cache-internal.xml" \
  "alephant-llm-kv-cache internal"

# 3) ai-gateway lib tests only (no integration tests/)
run_nextest "ai-gateway lib external" \
  cargo nextest run --profile ci-gate --lib -p ai-gateway --features external
copy_junit ci-gate "$RUN_DIR/gate/junit-ai-gateway-lib-external.xml"
enrich_junit_from_libtest "$RUN_DIR/gate/junit-ai-gateway-lib-external.xml" \
  "ai-gateway lib external"
append_junit_captures_to_log "$RUN_DIR/gate/junit-ai-gateway-lib-external.xml" \
  "ai-gateway lib external"

captures_json="$RUN_DIR/gate/lib-test-captures.json"
python3 "$SCRIPT_DIR/gate_junit_captures.py" write-json \
  --out "$captures_json" \
  --libtest-events "$libtest_jsonl" \
  "$RUN_DIR/gate/junit-small-crates.xml" "small workspace libs" \
  "$RUN_DIR/gate/junit-alephant-llm-kv-cache-external.xml" "alephant-llm-kv-cache external" \
  "$RUN_DIR/gate/junit-alephant-llm-kv-cache-internal.xml" "alephant-llm-kv-cache internal" \
  "$RUN_DIR/gate/junit-ai-gateway-lib-external.xml" "ai-gateway lib external"
echo "[gate] lib test captures JSON: $captures_json"

# manifest: includes commands array
tmp_cmds="$(mktemp)"
jq -n \
  --argjson c "$(printf '%s\n' "${cmds[@]}" | jq -R . | jq -s .)" \
  --arg cap "gate/lib-test-captures.json" \
  --arg lt "gate/libtest-events.jsonl" \
  '{
    commands: $c,
    coverage: { gate: "skipped_by_design" },
    gate_lib_test_captures_json: $cap,
    gate_libtest_events_jsonl: $lt,
    gate_nextest_libtest_json_note: "Each nextest segment runs with NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 and --message-format libtest-json-plus, multiplexed by gate_nextest_human_json_mux.py"
  }' \
  >"$tmp_cmds"
export TIER=gate
export EXTRA_JSON="$tmp_cmds"
bash "$SCRIPT_DIR/write-manifest.sh"
rm -f "$tmp_cmds"

echo "Gate artifacts under: $RUN_DIR"
