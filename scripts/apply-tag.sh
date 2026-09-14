#!/bin/sh
set -eu

export LC_ALL=C

fail() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

repo_root=$(git rev-parse --show-toplevel) || fail "not inside a Git worktree"
config_file="$repo_root/addon/config.yaml"
[ -f "$config_file" ] || fail "missing addon/config.yaml"

candidate=$(git rev-parse HEAD) || fail "cannot resolve HEAD"
derived_tag=$(
  git -c core.abbrev=8 show -s \
    --format='%cd-%h' \
    --date=format:%Y.%m.%d \
    "$candidate"
) || fail "cannot derive the release tag from HEAD"

if ! printf '%s\n' "$derived_tag" | grep -Eq '^20[0-9]{2}\.[0-9]{2}\.[0-9]{2}-[0-9a-f]{8,}$'; then
  fail "derived release tag is malformed: $derived_tag"
fi

if [ "${TAG_NAME+x}" = x ] && [ "$TAG_NAME" != "$derived_tag" ]; then
  fail "TAG_NAME must equal the tag derived from HEAD: $derived_tag"
fi
TAG_NAME=$derived_tag

version_count=$(awk '/^version:/ { count += 1 } END { print count + 0 }' "$config_file")
[ "$version_count" -eq 1 ] || fail "addon/config.yaml must contain exactly one root version key"

tmp_file=$(mktemp "$config_file.tmp.XXXXXX") || fail "cannot create a temporary config file"
cleanup() {
  [ -z "$tmp_file" ] || rm -f "$tmp_file"
}
trap cleanup 0
trap 'exit 130' HUP INT TERM

awk -v tag="$TAG_NAME" '
  /^version:/ { print "version: \"" tag "\""; next }
  { print }
' "$config_file" > "$tmp_file"
chmod 0644 "$tmp_file"

written_version=$(sed -n 's/^version: *"\([^"]*\)".*/\1/p' "$tmp_file")
[ "$written_version" = "$TAG_NAME" ] || fail "failed to write the derived add-on version"

mv "$tmp_file" "$config_file"
tmp_file=
