#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
cargo build -p gateway-e2e-harness
cargo run -p gateway-e2e-harness -- --profile gate --dotenv-path "${DOTENV_PATH:-$ROOT/.env}"
