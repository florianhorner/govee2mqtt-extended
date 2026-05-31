#!/bin/sh
# This script sets things up to make a release,
# creating a tag based on the current commit.
TAG_NAME=${TAG_NAME:-$(git -c "core.abbrev=8" show -s "--format=%cd-%h" "--date=format:%Y.%m.%d")}
git tag $TAG_NAME
./scripts/apply-tag.sh
docker run -t \
  -v "$(git rev-parse --git-common-dir):/git-common" \
  -v "$(pwd)":/app/ \
  -e GIT_DIR=/git-common \
  "ghcr.io/orhun/git-cliff/git-cliff:${TAG:-latest}" \
  --latest -o /app/addon/CHANGELOG.md -c /app/scripts/cliff.toml
git add addon/config.yaml addon/CHANGELOG.md
# --no-verify + Policy-Override: the fixed "Tag <name>" subject predates the
# commit-message-standards rule (commit-lint), which would otherwise reject it.
# cliff.toml skips "Tag 20.*" so this stays out of the changelog. CI records the
# override and does not block.
git commit --no-verify \
  -m "Tag $TAG_NAME" \
  -m "Policy-Override: release tag commit uses the fixed tag-release.sh subject format, which predates commit-message-standards."

# Publish with a SINGLE-tag push, never 'git push --tags'. Pushing many tags at
# once (e.g. stale upstream tags) makes GitHub suppress the tag workflow run, so
# the addon-publish job never fires and the versioned image is never built.
echo
echo "Tagged $TAG_NAME. To publish, push the branch and JUST this tag:"
echo "  git push origin main && git push origin refs/tags/$TAG_NAME"
