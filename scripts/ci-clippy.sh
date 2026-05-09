#!/usr/bin/env bash
# Matches the clippy job in `.github/workflows/rust-ci.yml` (local: default `cargo` toolchain).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo clippy -p ai-gateway --features external --all-targets -- -D warnings
cargo clippy -p alephant-llm-kv-cache --features external --all-targets -- -D warnings
cargo clippy -p alephant-llm-kv-cache --features internal --all-targets -- -D warnings
# `test` / `mock-server` depend on `ai-gateway`; workspace `--all-features` enables conflicting features, so exclude them.
cargo clippy --workspace --all-features --exclude ai-gateway --exclude alephant-llm-kv-cache --exclude test --exclude mock-server --all-targets -- -D warnings
