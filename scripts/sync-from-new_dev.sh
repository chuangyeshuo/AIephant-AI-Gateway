#!/usr/bin/env bash

set -euo pipefail

SOURCE_BRANCH="new_dev"
TARGET_BRANCH=""

usage() {
  cat <<'EOF'
Usage:
  ./scripts/sync-from-new_dev.sh
  ./scripts/sync-from-new_dev.sh -t new_dev-jonny
  ./scripts/sync-from-new_dev.sh -s new_dev -t your-branch

Notes:
  - Default source branch: new_dev
  - Default target branch: current branch
  - Steps: fetch remote source branch -> checkout target -> merge origin/<source>
  - Does not push automatically
  - Does not rebase automatically
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -s|--source)
      [[ $# -ge 2 ]] || { echo "Missing source branch name"; exit 1; }
      SOURCE_BRANCH="$2"
      shift 2
      ;;
    -t|--target)
      [[ $# -ge 2 ]] || { echo "Missing target branch name"; exit 1; }
      TARGET_BRANCH="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1"
      usage
      exit 1
      ;;
  esac
done

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "Current directory is not a Git repository"
  exit 1
fi

CURRENT_BRANCH="$(git branch --show-current)"

if [[ -z "$TARGET_BRANCH" ]]; then
  TARGET_BRANCH="$CURRENT_BRANCH"
fi

if [[ -z "$TARGET_BRANCH" ]]; then
  echo "Cannot determine current branch. Use -t to specify the target branch."
  exit 1
fi

if [[ "$SOURCE_BRANCH" == "$TARGET_BRANCH" ]]; then
  echo "Source and target branches must differ: $SOURCE_BRANCH"
  exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
  echo "Working tree has uncommitted changes; commit or stash before syncing."
  exit 1
fi

if ! git show-ref --verify --quiet "refs/heads/$TARGET_BRANCH"; then
  echo "Target branch does not exist locally: $TARGET_BRANCH"
  exit 1
fi

echo "==> Fetching remote branch origin/$SOURCE_BRANCH"
git fetch origin "$SOURCE_BRANCH"

if ! git show-ref --verify --quiet "refs/remotes/origin/$SOURCE_BRANCH"; then
  echo "Remote branch does not exist: origin/$SOURCE_BRANCH"
  exit 1
fi

if [[ "$CURRENT_BRANCH" != "$TARGET_BRANCH" ]]; then
  echo "==> Checking out target branch $TARGET_BRANCH"
  git switch "$TARGET_BRANCH"
fi

echo "==> Merging origin/$SOURCE_BRANCH into $TARGET_BRANCH"
git merge --no-edit "origin/$SOURCE_BRANCH"

echo "==> Sync complete"
echo "Current branch: $(git branch --show-current)"
