#!/usr/bin/env bash
# Smoke test for cloud log bodies: calls resolve_cloud_log_bodies against local MinIO (S3-compatible).
#
# Defaults: MinIO API on 9999, minioadmin / minioadmin. Create bucket
# "request-response-storage" (or set MINIO_BUCKET), or rely on the example’s auto-create.
#
# Usage:
#   ./scripts/cloud_log_bodies_minio_smoke.sh
#   MINIO_ENDPOINT=http://127.0.0.1:9000 ./scripts/cloud_log_bodies_minio_smoke.sh

set -euo pipefail
cd "$(dirname "$0")/.."

export CLOUD_LOG_STORAGE_TEST=1
export STORAGE_BACKEND=minio
export MINIO_ENDPOINT="${MINIO_ENDPOINT:-http://127.0.0.1:9999}"
export MINIO_ACCESS_KEY="${MINIO_ACCESS_KEY:-minioadmin}"
export MINIO_SECRET_KEY="${MINIO_SECRET_KEY:-minioadmin}"

# Default single build job to cap CPU/RAM; set CARGO_BUILD_JOBS=2 if needed
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"

exec cargo run -p ai-gateway --example cloud_log_bodies_real_storage
