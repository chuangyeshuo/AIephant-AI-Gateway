# Scripts Directory

This directory contains various testing and utility scripts for the Alephant AI Gateway project.

## `sync-from-new_dev.sh`

Syncs the latest `origin/new_dev` into your current dev branch, e.g. `new_dev-jonny`.

### Run manually

```bash
./scripts/sync-from-new_dev.sh
```

If you are not on the target branch, pass it explicitly:

```bash
./scripts/sync-from-new_dev.sh -t new_dev-jonny
```

If the source branch is not `new_dev`, use:

```bash
./scripts/sync-from-new_dev.sh -s other-source-branch -t new_dev-jonny
```

### What the script does

- Default source branch: `new_dev`
- Default target branch: current branch
- Requires a clean working tree before running
- Only `fetch` + `merge`
- Does not `push` automatically
- Does not `rebase` automatically

### Cron example

To sync once a day at 10:00, add a `crontab` entry like:

```bash
0 10 * * * cd /Users/lijiayi/Documents/rustProject/work-alephant-ai-gateway && ./scripts/sync-from-new_dev.sh >> /tmp/work-alephant-sync.log 2>&1
```

List cron jobs:

```bash
crontab -l
```

Edit cron jobs:

```bash
crontab -e
```

## Contents

### `mock_openai_8011.py`

OpenAI-compatible local mock upstream for gateway debugging.

Default bind:

```bash
python3 scripts/mock_openai_8011.py
```

Default address:

```text
http://127.0.0.1:8011
```

Supported endpoints:

- `GET /v1/models`
- `GET /v1/v1/models`
- `POST /v1/chat/completions`
- `POST /v1/v1/chat/completions`

Features:

- non-stream chat completion
- stream chat completion via SSE
- compatibility with accidental double-`/v1` upstream paths

### `trace-test-client/`

A manual testing tool for tracing and propagation functionality. This tool runs against a real environment and is used for manual testing of tracing features.

### `test/`

Integration tests and test utilities for the Alephant AI Gateway project. These tests are designed to run against a real or mocked environment.

## Usage

### Running trace-test-client

```bash
cd scripts/trace-test-client
cargo run

cd scripts/test
cargo run
```

### Running Unit Tests

```bash
cd ai-gateway
cargo test --features testing
```
