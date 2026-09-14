#!/bin/sh
set -eu

export LC_ALL=C

expected_repository=florianhorner/govee2mqtt-extended
expected_owner=florianhorner
expected_parent_owner=wez
release_remote=origin
release_branch=main
git_cliff_version=2.13.1
git_cliff_image='ghcr.io/orhun/git-cliff/git-cliff:2.13.1@sha256:d49216b61658fc1b10bab6c5f82dfca03b8e37278618fdc3db235d95cf3c33f5'

mode=check
mode_was_set=false
expected_head=
tmp_dir=
tmp_changelog=
mutation_started=false
candidate=
work_branch=

usage() {
  cat <<'EOF'
Usage:
  scripts/prepare-release.sh --check [--expected-head <full-sha>]
  scripts/prepare-release.sh --prepare --expected-head <full-sha>

--check is the default and does not change Git refs or tracked files.
--prepare requires an attached non-main branch, runs the full local gates,
and creates one two-file metadata commit.
It never creates or pushes a tag and never creates a GitHub Release.
EOF
}

fail() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  exit_code=$?
  trap - 0 HUP INT TERM

  if [ "$exit_code" -ne 0 ] && [ "$mutation_started" = true ]; then
    rollback_failed=false
    if rollback_head=$(git rev-parse HEAD 2>/dev/null); then
      if [ "$rollback_head" != "$candidate" ]; then
        if ! git reset --mixed "$candidate" >/dev/null 2>&1; then
          printf 'ROLLBACK FAILED: could not restore HEAD to %s\n' "$candidate" >&2
          rollback_failed=true
        fi
      fi
    else
      printf 'ROLLBACK FAILED: could not inspect HEAD\n' >&2
      rollback_failed=true
    fi

    if ! git restore --staged --worktree -- addon/config.yaml addon/CHANGELOG.md >/dev/null 2>&1; then
      printf 'ROLLBACK FAILED: could not restore release metadata files\n' >&2
      rollback_failed=true
    fi

    if rollback_head=$(git rev-parse HEAD 2>/dev/null); then
      if [ "$rollback_head" != "$candidate" ]; then
        printf 'ROLLBACK FAILED: HEAD is %s, expected %s\n' "$rollback_head" "$candidate" >&2
        rollback_failed=true
      fi
    else
      printf 'ROLLBACK FAILED: could not verify HEAD\n' >&2
      rollback_failed=true
    fi

    if rollback_state=$(git status --porcelain=v1 --untracked-files=normal 2>/dev/null); then
      if [ -n "$rollback_state" ]; then
        printf 'ROLLBACK FAILED: worktree or index is not clean:\n%s\n' "$rollback_state" >&2
        rollback_failed=true
      fi
    else
      printf 'ROLLBACK FAILED: could not verify the worktree and index\n' >&2
      rollback_failed=true
    fi

    if [ "$rollback_failed" = true ]; then
      printf 'ROLLBACK FAILED: inspect this worktree before continuing.\n' >&2
    fi
  fi

  if [ -n "$tmp_changelog" ]; then
    rm -f "$tmp_changelog"
  fi
  if [ -n "$tmp_dir" ]; then
    rmdir "$tmp_dir" 2>/dev/null || true
  fi
  exit "$exit_code"
}
trap cleanup 0
trap 'exit 130' HUP INT TERM

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check)
      [ "$mode_was_set" = false ] || fail "choose exactly one of --check or --prepare"
      mode=check
      mode_was_set=true
      ;;
    --prepare)
      [ "$mode_was_set" = false ] || fail "choose exactly one of --check or --prepare"
      mode=prepare
      mode_was_set=true
      ;;
    --expected-head)
      shift
      [ "$#" -gt 0 ] || fail "--expected-head requires a full commit SHA"
      expected_head=$1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
  shift
done

case "$expected_head" in
  '')
    [ "$mode" = check ] || fail "--prepare requires --expected-head with the full candidate SHA"
    ;;
  *[!0-9a-f]*) fail "--expected-head must be a lowercase hexadecimal SHA" ;;
  *) [ "${#expected_head}" -eq 40 ] || fail "--expected-head must contain the full 40-character SHA" ;;
esac

for command_name in git gh awk sed grep mktemp sort; do
  command -v "$command_name" >/dev/null 2>&1 || fail "required command is unavailable: $command_name"
done

repo_root=$(git rev-parse --show-toplevel) || fail "not inside a Git worktree"
cd "$repo_root"
[ -f addon/config.yaml ] || fail "missing addon/config.yaml"
[ -f addon/CHANGELOG.md ] || fail "missing addon/CHANGELOG.md"
[ -f scripts/cliff.toml ] || fail "missing scripts/cliff.toml"

remote_url=$(git remote get-url "$release_remote") || fail "missing $release_remote remote"

is_owned_remote_url() {
  case "$1" in
    https://github.com/florianhorner/govee2mqtt-extended|https://github.com/florianhorner/govee2mqtt-extended.git|git@github.com:florianhorner/govee2mqtt-extended.git|ssh://git@github.com/florianhorner/govee2mqtt-extended.git) return 0 ;;
    *) return 1 ;;
  esac
}

is_owned_remote_url "$remote_url" || fail "$release_remote fetch URL does not point at the owned fork: $remote_url"
push_urls=$(git remote get-url --push --all "$release_remote") || fail "cannot read $release_remote push URLs"
[ -n "$push_urls" ] || fail "$release_remote has no push URL"
while IFS= read -r push_url; do
  [ -n "$push_url" ] || fail "$release_remote returned an empty push URL"
  is_owned_remote_url "$push_url" || fail "$release_remote push URL does not point at the owned fork: $push_url"
done <<EOF
$push_urls
EOF

repo_identity=$(
  gh repo view "$expected_repository" \
    --json nameWithOwner,owner,isFork,parent \
    --jq '[.nameWithOwner, .owner.login, (.isFork | tostring), .parent.owner.login] | join("|")'
) || fail "cannot verify GitHub repository ownership"
repo_identity_count=$(printf '%s\n' "$repo_identity" | awk -F '|' 'NF == 4 { print 4 }')
[ "$repo_identity_count" = 4 ] || fail "GitHub returned malformed repository identity"
repo_name=$(printf '%s\n' "$repo_identity" | awk -F '|' '{ print $1 }')
repo_owner=$(printf '%s\n' "$repo_identity" | awk -F '|' '{ print $2 }')
repo_is_fork=$(printf '%s\n' "$repo_identity" | awk -F '|' '{ print $3 }')
repo_parent_owner=$(printf '%s\n' "$repo_identity" | awk -F '|' '{ print $4 }')
[ "$repo_name" = "$expected_repository" ] || fail "unexpected GitHub repository: $repo_name"
[ "$repo_owner" = "$expected_owner" ] || fail "repository is not owned by $expected_owner"
[ "$repo_is_fork" = true ] || fail "expected $expected_repository to remain a fork"
[ "$repo_parent_owner" = "$expected_parent_owner" ] || fail "unexpected upstream owner: $repo_parent_owner"

assert_clean() {
  worktree_state=$(git status --porcelain=v1 --untracked-files=normal) ||
    fail "cannot inspect the worktree and index"
  [ -z "$worktree_state" ] || {
    printf '%s\n' "$worktree_state" >&2
    fail "release preparation requires a completely clean worktree and index"
  }
}

assert_prepare_branch() {
  current_branch=$(git symbolic-ref --quiet --short HEAD) ||
    fail "--prepare requires an attached release-preparation branch"
  [ "$current_branch" != "$release_branch" ] ||
    fail "--prepare must not create the metadata commit directly on $release_branch"
  if [ -n "$work_branch" ]; then
    [ "$current_branch" = "$work_branch" ] ||
      fail "release-preparation branch moved: expected $work_branch, found $current_branch"
  else
    work_branch=$current_branch
  fi
}

read_remote_head() {
  remote_output=$(git ls-remote --exit-code "$release_remote" "refs/heads/$release_branch") ||
    fail "cannot read $release_remote/$release_branch"
  remote_head=$(printf '%s\n' "$remote_output" | awk 'NR == 1 { value = $1 } END { if (NR != 1 || value == "") exit 1; print value }') ||
    fail "$release_remote/$release_branch did not resolve to exactly one commit"
  printf '%s\n' "$remote_head"
}

assert_tag_absent() {
  tag_to_check=$1
  if git show-ref --verify --quiet "refs/tags/$tag_to_check"; then
    fail "release tag already exists locally: $tag_to_check"
  fi
  remote_tag_output=$(git ls-remote --tags "$release_remote" "refs/tags/$tag_to_check" "refs/tags/$tag_to_check^{}") ||
    fail "cannot verify whether the release tag exists remotely"
  [ -z "$remote_tag_output" ] || fail "release tag already exists remotely: $tag_to_check"
}

assert_clean
candidate=$(git rev-parse HEAD) || fail "cannot resolve HEAD"
remote_head=$(read_remote_head)
[ "$candidate" = "$remote_head" ] || fail "HEAD $candidate is not the current $release_remote/$release_branch $remote_head"
if [ -n "$expected_head" ]; then
  [ "$candidate" = "$expected_head" ] || fail "HEAD moved: expected $expected_head, found $candidate"
fi
if [ "$mode" = prepare ]; then
  assert_prepare_branch
fi

tag_name=$(
  git -c core.abbrev=8 show -s \
    --format='%cd-%h' \
    --date=format:%Y.%m.%d \
    "$candidate"
) || fail "cannot derive a release tag from the candidate"
if ! printf '%s\n' "$tag_name" | grep -Eq '^20[0-9]{2}\.[0-9]{2}\.[0-9]{2}-[0-9a-f]{8,}$'; then
  fail "derived release tag is malformed: $tag_name"
fi
if [ "${TAG_NAME+x}" = x ] && [ "$TAG_NAME" != "$tag_name" ]; then
  fail "TAG_NAME must equal the immutable tag derived from HEAD: $tag_name"
fi
assert_tag_absent "$tag_name"

remote_release_tags=$(git ls-remote --refs --tags "$release_remote" 'refs/tags/20*') ||
  fail "cannot read the remote release-tag inventory"
remote_release_inventory=$(
  printf '%s\n' "$remote_release_tags" |
    awk '{ sub("refs/tags/", "", $2); print $1 " " $2 }' |
    LC_ALL=C sort
)
local_release_inventory=$(
  git for-each-ref --format='%(objectname) %(refname:strip=2)' 'refs/tags/20*' |
    LC_ALL=C sort
)
[ -n "$remote_release_inventory" ] || fail "the remote has no release-tag baseline"
[ "$local_release_inventory" = "$remote_release_inventory" ] ||
  fail "local release tags differ from the remote; synchronize tags before continuing"

previous_tag=$(git describe --tags --match '20*' --abbrev=0 "$candidate") ||
  fail "cannot find the previous release tag"
[ "$previous_tag" != "$tag_name" ] || fail "candidate is already tagged as $tag_name"

latest_release_tag=$(
  gh release list --repo "$expected_repository" --limit 100 \
    --json tagName,isLatest \
    --jq '.[] | select(.isLatest == true) | .tagName'
) || fail "cannot read the latest GitHub Release"
[ -n "$latest_release_tag" ] || fail "GitHub has no Latest release baseline"
latest_release_count=$(printf '%s\n' "$latest_release_tag" | awk 'NF { count += 1 } END { print count + 0 }')
[ "$latest_release_count" -eq 1 ] || fail "GitHub returned more than one Latest release"
git show-ref --verify --quiet "refs/tags/$latest_release_tag" ||
  fail "the Latest GitHub Release tag is missing locally: $latest_release_tag"
git merge-base --is-ancestor "$latest_release_tag" "$candidate" ||
  fail "the Latest GitHub Release is not an ancestor of the candidate"

ci_run=$(
  gh run list --repo "$expected_repository" \
    --workflow build.yml \
    --branch "$release_branch" \
    --commit "$candidate" \
    --limit 20 \
    --json databaseId,status,conclusion,headSha,url \
    --jq '.[0] | [.databaseId, .status, .conclusion, .headSha, .url] | join("|")'
) || fail "cannot read Container Build status for $candidate"
[ -n "$ci_run" ] && [ "$ci_run" != '||||' ] || fail "no Container Build run exists for $candidate"
ci_field_count=$(printf '%s\n' "$ci_run" | awk -F '|' 'NF == 5 { print 5 }')
[ "$ci_field_count" = 5 ] || fail "GitHub returned malformed Container Build data"
ci_run_id=$(printf '%s\n' "$ci_run" | awk -F '|' '{ print $1 }')
ci_status=$(printf '%s\n' "$ci_run" | awk -F '|' '{ print $2 }')
ci_conclusion=$(printf '%s\n' "$ci_run" | awk -F '|' '{ print $3 }')
ci_head=$(printf '%s\n' "$ci_run" | awk -F '|' '{ print $4 }')
ci_url=$(printf '%s\n' "$ci_run" | awk -F '|' '{ print $5 }')
[ "$ci_head" = "$candidate" ] || fail "Container Build is not bound to the candidate SHA"
[ "$ci_status" = completed ] || fail "Container Build has not completed: $ci_url"
[ "$ci_conclusion" = success ] || fail "Container Build did not succeed: $ci_url"

ci_jobs=$(
  gh run view "$ci_run_id" --repo "$expected_repository" \
    --json jobs \
    --jq '.jobs[] | [.name, .status, .conclusion] | join("|")'
) || fail "cannot read Container Build jobs: $ci_url"

require_successful_job() {
  job_prefix=$1
  if ! printf '%s\n' "$ci_jobs" | awk -F '|' -v prefix="$job_prefix" '
    index($1, prefix) == 1 {
      count += 1
      if ($2 == "completed" && $3 == "success") success += 1
    }
    END { exit !(count == 1 && success == 1) }
  '; then
    fail "required Container Build job is missing or not successful: $job_prefix"
  fi
}

require_successful_job 'build (linux/amd64)'
require_successful_job 'build (linux/arm64)'
require_successful_job 'merge'
require_successful_job 'test-addon (amd64,'
require_successful_job 'test-addon (aarch64,'

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/govee-release.XXXXXX") || fail "cannot create a temporary directory"
tmp_changelog="$tmp_dir/CHANGELOG.md"
git_common_dir=$(git rev-parse --path-format=absolute --git-common-dir) || fail "cannot resolve the shared Git directory"

generate_changelog() {
  if command -v git-cliff >/dev/null 2>&1 && [ "$(git-cliff --version 2>/dev/null || true)" = "git-cliff $git_cliff_version" ]; then
    if ! git-cliff \
      --offline \
      --repository "$git_common_dir" \
      --config "$repo_root/scripts/cliff.toml" \
      --tag "$tag_name" \
      "$previous_tag..$candidate" > "$tmp_changelog"; then
      return 1
    fi
    return 0
  fi

  command -v docker >/dev/null 2>&1 ||
    fail "install git-cliff $git_cliff_version or make Docker available"
  if ! docker run --rm --network none \
    --mount "type=bind,src=$git_common_dir,dst=/repo.git,readonly" \
    --mount "type=bind,src=$repo_root/scripts/cliff.toml,dst=/cliff.toml,readonly" \
    "$git_cliff_image" \
    --offline \
    --repository /repo.git \
    --config /cliff.toml \
    --tag "$tag_name" \
    "$previous_tag..$candidate" > "$tmp_changelog"; then
    return 1
  fi
  return 0
}

generate_changelog || fail "git-cliff failed to generate the candidate changelog"
[ -s "$tmp_changelog" ] || fail "git-cliff generated an empty changelog"
changelog_headers=$(awk '$0 == "# Changelog" { count += 1 } END { print count + 0 }' "$tmp_changelog")
release_headers=$(awk '/^## \[/ { count += 1 } END { print count + 0 }' "$tmp_changelog")
target_headers=$(awk -v prefix="## [$tag_name] - " 'index($0, prefix) == 1 { count += 1 } END { print count + 0 }' "$tmp_changelog")
footer_count=$(awk '$0 == "<!-- generated by git-cliff -->" { count += 1 } END { print count + 0 }' "$tmp_changelog")
[ "$changelog_headers" -eq 1 ] || fail "generated changelog must contain exactly one title"
[ "$release_headers" -eq 1 ] || fail "generated changelog must contain exactly one release section"
[ "$target_headers" -eq 1 ] || fail "generated changelog does not target $tag_name exactly once"
[ "$footer_count" -eq 1 ] || fail "generated changelog must contain exactly one generated footer"
if grep -Fqi '[unreleased]' "$tmp_changelog"; then
  fail "generated changelog still contains an unreleased section"
fi
if grep -Eq '(^|^- )Merge pull request' "$tmp_changelog"; then
  fail "generated changelog contains an uncurated merge commit"
fi

printf '\nRelease candidate\n'
printf '  source:              %s\n' "$candidate"
printf '  add-on baseline:     %s\n' "$previous_tag"
printf '  public-notes base:   %s\n' "$latest_release_tag"
printf '  proposed tag:        %s\n' "$tag_name"
printf '  exact-head CI:       %s\n' "$ci_url"
printf '\nGenerated add-on changelog preview\n\n'
sed -n '1,240p' "$tmp_changelog"

if [ "$mode" = check ]; then
  printf '\nCHECK PASSED — no tag, commit, push, release, or tracked-file change was made.\n'
  printf 'To prepare the metadata commit after review:\n'
  printf '  scripts/prepare-release.sh --prepare --expected-head %s\n' "$candidate"
  mutation_started=false
  exit 0
fi

printf '\nRunning release gates against %s\n' "$candidate"
command -v cargo >/dev/null 2>&1 || fail "required command is unavailable: cargo"
cargo build --all
cargo clippy --all -- -D warnings
cargo clippy --all --all-features -- -D warnings
cargo test --all -- --show-output --test-threads=1
cargo test --all --all-features -- --show-output --test-threads=1
python3 -m unittest scripts/test_live_2fa.py scripts/test_prepare_release.py
cargo fmt --all -- --check

# Close the time-of-check/time-of-use window before changing tracked files.
assert_clean
[ "$(git rev-parse HEAD)" = "$candidate" ] || fail "HEAD moved while release gates were running"
assert_prepare_branch
[ "$(read_remote_head)" = "$candidate" ] || fail "$release_remote/$release_branch moved while release gates were running"
assert_tag_absent "$tag_name"

mutation_started=true
cp "$tmp_changelog" addon/CHANGELOG.md
TAG_NAME=$tag_name ./scripts/apply-tag.sh

changed_files=$(git diff --name-only | LC_ALL=C sort)
expected_files=$(printf '%s\n' addon/CHANGELOG.md addon/config.yaml | LC_ALL=C sort)
[ "$changed_files" = "$expected_files" ] || fail "release preparation changed files outside the two-file metadata contract"
[ -z "$(git diff --cached --name-only)" ] || fail "release preparation unexpectedly staged files before its commit"

git add -- addon/CHANGELOG.md addon/config.yaml
git commit --only \
  -m "chore(release): prepare $tag_name" \
  -m "NOCHANGELOG
Release-Candidate: $candidate" \
  -- addon/CHANGELOG.md addon/config.yaml

release_commit=$(git rev-parse HEAD)
[ "$(git rev-parse HEAD^)" = "$candidate" ] || fail "release metadata commit is not a direct child of the candidate"
committed_files=$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)
[ "$committed_files" = "$expected_files" ] || fail "release metadata commit contains an unexpected write set"
assert_clean
if git show-ref --verify --quiet "refs/tags/$tag_name"; then
  fail "release preparation must not create a shared local tag"
fi

mutation_started=false
printf '\nPREPARE PASSED\n'
printf '  candidate:       %s\n' "$candidate"
printf '  metadata commit: %s\n' "$release_commit"
printf '  tag reserved:    %s (not created)\n' "$tag_name"
printf '\nNext: open the metadata PR and get it green, but do not merge it yet.\n'
printf 'Immediately before tagging, reverify the PR is mergeable with exactly two files,\n'
printf 'origin/main is still the candidate, the tag is absent, and every push URL is owned.\n'
printf 'After publication is explicitly approved, push only this candidate tag:\n'
printf '  git tag %s %s\n' "$tag_name" "$candidate"
printf '  git push %s refs/tags/%s:refs/tags/%s\n' "$release_remote" "$tag_name" "$tag_name"
printf 'Require both tag-only add-on jobs and both versioned images to succeed.\n'
printf '  ghcr.io/florianhorner/govee2mqtt-amd64:%s\n' "$tag_name"
printf '  ghcr.io/florianhorner/govee2mqtt-aarch64:%s\n' "$tag_name"
printf 'Only then merge the metadata PR so addon/config.yaml advertises images that exist.\n'
printf 'Create or publish the curated GitHub Release last.\n'
