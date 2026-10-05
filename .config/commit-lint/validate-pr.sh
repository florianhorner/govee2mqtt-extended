#!/usr/bin/env bash
# AUTO-GENERATED — DO NOT EDIT BY HAND.
# Managed by commit-lint-kit-v1.
#
# Checks every non-merge commit of a pull request and the pull request title
# with the commit-msg script next to this file. On a runner the inputs are read
# from the event file GitHub provides (GITHUB_EVENT_PATH), so pull request text
# is neither interpolated into shell code nor printed with the step's
# environment. Run by hand, the same inputs come from these variables, which
# take precedence when set:
#
#   PR_BASE_SHA      base revision of the pull request
#   PR_HEAD_SHA      head revision of the pull request (optional)
#   PUSH_BEFORE_SHA  previous tip for push events (optional)
#   PR_TITLE         pull request title (optional)
#   PR_AUTHOR        login of the pull request author (optional)
#
# Exit status: 0 policy passed, 1 policy violations, 2 unusable commit range.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK="$HERE/commit-msg"

# Pull requests opened by these logins may skip the rule IDs listed below,
# for the pull request title only.
TRUSTED_BOTS='renovate[bot]
dependabot[bot]
pre-commit-ci[bot]
app/github-actions'
BOT_SKIPS='WHY_REQUIRED
SUBJECT_TOO_LONG'

EVENT_FILE="${GITHUB_EVENT_PATH:-}"
if [ -n "$EVENT_FILE" ] && ! command -v jq >/dev/null 2>&1; then
  echo "::error::jq is required to read the event file."
  exit 2
fi

event_field() {
  [ -n "$EVENT_FILE" ] && [ -f "$EVENT_FILE" ] || return 0
  jq -r "$1 // empty" "$EVENT_FILE" 2>/dev/null || true
}

PR_BASE_SHA="${PR_BASE_SHA:-$(event_field '.pull_request.base.sha')}"
PR_HEAD_SHA="${PR_HEAD_SHA:-$(event_field '.pull_request.head.sha')}"
PUSH_BEFORE_SHA="${PUSH_BEFORE_SHA:-$(event_field '.before')}"
PR_TITLE="${PR_TITLE:-$(event_field '.pull_request.title')}"
PR_AUTHOR="${PR_AUTHOR:-$(event_field '.pull_request.user.login')}"
SUMMARY="${GITHUB_STEP_SUMMARY:-/dev/null}"

if [ ! -f "$HOOK" ]; then
  echo "::error::Missing commit-msg script next to validate-pr.sh."
  exit 2
fi

SCRATCH_DIR="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/commit-policy.XXXXXX")" || exit 2
chmod 700 "$SCRATCH_DIR"
trap 'rm -rf "$SCRATCH_DIR"' EXIT
mkdir -p "$SCRATCH_DIR/messages" "$SCRATCH_DIR/results"

# Everything checked here is committed text or a pull request title, so a
# line that starts with `#` is part of the message, not an editor comment.
run_hook() {
  if command -v timeout >/dev/null 2>&1; then
    COMMIT_LINT_COMMITTED=1 timeout 30s bash "$HOOK" "$1"
  else
    COMMIT_LINT_COMMITTED=1 bash "$HOOK" "$1"
  fi
}

# --- Commit range -----------------------------------------------------------

HEAD_SHA="$(git rev-parse HEAD)" || exit 2
if [ -n "$PR_HEAD_SHA" ]; then
  if ! printf '%s\n' "$PR_HEAD_SHA" | grep -Eq '^[0-9a-f]{40}$' \
    || ! git cat-file -e "${PR_HEAD_SHA}^{commit}" 2>/dev/null \
    || ! git merge-base --is-ancestor "$PR_HEAD_SHA" "$HEAD_SHA"; then
    echo "::error::Pull-request head is absent from the checkout."
    exit 2
  fi
fi
if [ -n "$PR_BASE_SHA" ]; then
  RANGE_BASE="$PR_BASE_SHA"
elif printf '%s\n' "$PUSH_BEFORE_SHA" | grep -Eq '^[0-9a-f]{40}$' \
  && [ "$PUSH_BEFORE_SHA" != "0000000000000000000000000000000000000000" ]; then
  RANGE_BASE="$PUSH_BEFORE_SHA"
elif git rev-parse --verify HEAD^ >/dev/null 2>&1; then
  RANGE_BASE="$(git rev-parse HEAD^)"
else
  RANGE_BASE="$HEAD_SHA"
fi

if ! git cat-file -e "${RANGE_BASE}^{commit}" 2>/dev/null; then
  echo "::error::Range base is not present in the checkout."
  exit 2
fi

# Judge only the commits the pull request adds. In the merge checkout that
# pull_request events provide, HEAD merges the current base tip (first parent)
# with the pull request head (second parent). The base revision recorded on the
# pull request can be older than that tip, so both are excluded; otherwise
# commits that already landed on the base branch would be judged as well.
RANGE_END="$HEAD_SHA"
EXCLUDED=("$RANGE_BASE")
if [ -n "$PR_HEAD_SHA" ] && [ "$PR_HEAD_SHA" != "$HEAD_SHA" ] \
  && [ "$(git rev-parse --verify -q "${HEAD_SHA}^2" 2>/dev/null)" = "$PR_HEAD_SHA" ]; then
  RANGE_END="$PR_HEAD_SHA"
  EXCLUDED+=("$(git rev-parse "${HEAD_SHA}^1")")
fi
RANGE_SHAS="$(git rev-list --no-merges "$RANGE_END" --not "${EXCLUDED[@]}")" || exit 2
COUNT="$(printf '%s\n' "$RANGE_SHAS" | grep -cv '^$' || true)"
echo "Validating $COUNT commits of ${RANGE_END} that the base does not contain"

# --- Policy -----------------------------------------------------------------

is_trusted_bot() {
  [ -n "${1:-}" ] && printf '%s\n' "$TRUSTED_BOTS" | grep -qxF -- "$1"
}

all_blocks_skippable() {
  local result_file="$1" ids id
  # shellcheck disable=SC2016
  ids="$(awk '/^BLOCK: / {print $2}' "$result_file")"
  [ -n "$ids" ] || return 1
  while IFS= read -r id; do
    [ -z "$id" ] && continue
    printf '%s\n' "$BOT_SKIPS" | grep -qxF -- "$id" || return 1
  done <<EOF_IDS
$ids
EOF_IDS
}

record_failure() {
  local ref="$1" result_file="$2" ids
  # shellcheck disable=SC2016
  ids="$(awk '/^BLOCK: / {printf "%s%s", separator, $2; separator=","}' "$result_file")"
  [ -n "$ids" ] || ids="UNKNOWN"
  printf '| %s | %s |\n' "$ref" "$ids" >> "$SCRATCH_DIR/failures.md"
}

FAIL_COUNT=0
: > "$SCRATCH_DIR/failures.md"

if [ -n "$PR_TITLE" ]; then
  printf '%s\n' "$PR_TITLE" > "$SCRATCH_DIR/messages/__pr_title__.txt"
  if ! run_hook "$SCRATCH_DIR/messages/__pr_title__.txt" \
    2> "$SCRATCH_DIR/results/__pr_title__.err"; then
    if is_trusted_bot "$PR_AUTHOR" \
      && all_blocks_skippable "$SCRATCH_DIR/results/__pr_title__.err"; then
      echo "PR title violations are skippable for the configured bot."
    else
      FAIL_COUNT=$((FAIL_COUNT + 1))
      record_failure "PR title" "$SCRATCH_DIR/results/__pr_title__.err"
    fi
  fi
fi

while IFS= read -r SHA; do
  [ -z "$SHA" ] && continue
  git log -1 --format=%B "$SHA" > "$SCRATCH_DIR/messages/${SHA}.txt"
  if ! run_hook "$SCRATCH_DIR/messages/${SHA}.txt" \
    2> "$SCRATCH_DIR/results/${SHA}.err"; then
    FAIL_COUNT=$((FAIL_COUNT + 1))
    record_failure "${SHA:0:12}" "$SCRATCH_DIR/results/${SHA}.err"
  fi
done <<EOF_SHAS
$RANGE_SHAS
EOF_SHAS

# --- Report -----------------------------------------------------------------
# Only refs and rule IDs are reported. Offending text is never echoed.

{
  echo "## Commit policy results"
  echo ""
  echo "Validated **$COUNT commits** plus the PR title when present."
  echo ""
  if [ "$FAIL_COUNT" -eq 0 ]; then
    echo ":white_check_mark: Commit policy passed."
  else
    echo ":x: **$FAIL_COUNT policy failures.**"
    echo ""
    echo "| ref | rule IDs |"
    echo "|---|---|"
    cat "$SCRATCH_DIR/failures.md"
  fi
} >> "$SUMMARY"

if [ "$FAIL_COUNT" -ne 0 ]; then
  echo "| ref | rule IDs |"
  cat "$SCRATCH_DIR/failures.md"
  echo "::error::Commit policy failed ($FAIL_COUNT failures). Check a message with: bash .config/commit-lint/commit-msg <message-file>"
  exit 1
fi
echo "Commit policy passed."
