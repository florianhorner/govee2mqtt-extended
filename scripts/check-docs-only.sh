#!/usr/bin/env bash
# Pre-commit hook: warn on docs-only commits (skip on docs-focused branches)
BRANCH=$(git branch --show-current 2>/dev/null || echo "")
if echo "$BRANCH" | grep -qiE "(docs|doc|readme|changelog)"; then
  exit 0
fi
STAGED=$(git diff --cached --name-only)
NON_DOCS=$(echo "$STAGED" | grep -v -E "^(README|CHANGELOG|CLAUDE|TODOS|docs/|addon/README|.*\.md$)" || true)
if [ -z "$NON_DOCS" ]; then
  echo "Warning: Docs-only commit. Consider batching with the next code change."
  echo "  Staged: $STAGED"
  echo "  To proceed anyway: git commit --no-verify"
  exit 1
fi
