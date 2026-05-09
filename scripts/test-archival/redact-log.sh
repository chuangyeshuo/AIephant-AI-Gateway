#!/usr/bin/env bash
set -euo pipefail
# Read stdin, write redacted log to stdout; huge bodies are not fully logged (caller sets RUST_LOG)
sed -E \
  's/(authorization:)[[:space:]]*.+/\1 [REDACTED]/Ig; s/(api-key:)[[:space:]]*.+/\1 [REDACTED]/Ig; s/(cookie:)[[:space:]]*.+/\1 [REDACTED]/Ig'
