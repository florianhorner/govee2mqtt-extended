#!/bin/sh
set -eu

# Compatibility entry point. Release preparation is deliberately tag-free;
# publishing a tag is a separate, explicitly approved operation.
script_dir=$(CDPATH='' cd -P "$(dirname "$0")" && pwd)
exec "$script_dir/prepare-release.sh" "$@"
