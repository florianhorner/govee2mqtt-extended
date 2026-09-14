#!/bin/sh
set -eu

export LC_ALL=C

release_remote=origin
release_branch=main

usage() {
  cat <<'EOF'
Usage: scripts/validate-release-publication.sh <release-tag>

Validates that a tag-triggered publication is named for the checked-out commit
and that the commit is still the exact remote main head. It performs no writes.
EOF
}

fail() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

[ "$#" -eq 1 ] || {
  usage >&2
  fail "expected exactly one release tag"
}
release_tag=$1

if ! printf '%s\n' "$release_tag" | grep -Eq '^20[0-9]{2}\.[0-9]{2}\.[0-9]{2}-[0-9a-f]{8,}$'; then
  fail "release tag is malformed: $release_tag"
fi

for command_name in git awk grep; do
  command -v "$command_name" >/dev/null 2>&1 ||
    fail "required command is unavailable: $command_name"
done

repo_root=$(git rev-parse --show-toplevel) || fail "not inside a Git worktree"
cd "$repo_root"

remote_url=$(git remote get-url "$release_remote") ||
  fail "missing $release_remote remote"
case "$remote_url" in
  https://github.com/florianhorner/govee2mqtt-extended|https://github.com/florianhorner/govee2mqtt-extended.git|git@github.com:florianhorner/govee2mqtt-extended.git|ssh://git@github.com/florianhorner/govee2mqtt-extended.git) ;;
  *) fail "$release_remote does not point at the owned fork: $remote_url" ;;
esac

candidate=$(git rev-parse HEAD) || fail "cannot resolve HEAD"
derived_tag=$(
  git -c core.abbrev=8 show -s \
    --format='%cd-%h' \
    --date=format:%Y.%m.%d \
    "$candidate"
) || fail "cannot derive the release tag from HEAD"
[ "$release_tag" = "$derived_tag" ] ||
  fail "release tag $release_tag does not match HEAD-derived tag $derived_tag"

remote_tag_output=$(
  git ls-remote --exit-code "$release_remote" \
    "refs/tags/$release_tag" "refs/tags/$release_tag^{}"
) || fail "cannot resolve release tag on $release_remote: $release_tag"
remote_tag_commit=$(
  printf '%s\n' "$remote_tag_output" |
    awk -v ref="refs/tags/$release_tag" '
      $2 == ref { direct_count += 1; direct = $1 }
      $2 == ref "^{}" { peeled_count += 1; peeled = $1 }
      END {
        if (direct_count != 1 || peeled_count > 1) exit 1
        if (peeled_count == 1) print peeled
        else print direct
      }
    '
) || fail "$release_remote release tag did not resolve to exactly one commit: $release_tag"
[ "$remote_tag_commit" = "$candidate" ] ||
  fail "$release_remote release tag resolves to $remote_tag_commit, expected $candidate"

remote_output=$(git ls-remote --exit-code "$release_remote" "refs/heads/$release_branch") ||
  fail "cannot read $release_remote/$release_branch"
remote_head=$(
  printf '%s\n' "$remote_output" |
    awk 'NR == 1 { value = $1 } END { if (NR != 1 || value == "") exit 1; print value }'
) || fail "$release_remote/$release_branch did not resolve to exactly one commit"
[ "$candidate" = "$remote_head" ] ||
  fail "tagged commit $candidate is not the current $release_remote/$release_branch $remote_head"

printf 'RELEASE PREFLIGHT PASSED\n'
printf '  tag:       %s\n' "$release_tag"
printf '  candidate: %s\n' "$candidate"
printf '  main:      %s\n' "$remote_head"
