#!/bin/sh
set -eu

# Compatibility entry point. Release preparation creates no tag. Publishing a tag
# requires separate explicit approval.
script_dir=$(CDPATH='' cd -P "$(dirname "$0")" && pwd)
exec "$script_dir/prepare-release.sh" "$@"
