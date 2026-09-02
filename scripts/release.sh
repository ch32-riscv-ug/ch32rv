#!/usr/bin/env bash
# release.sh - version bump used on EVERY release (called by .github/workflows/release.yml).
#
# NOT a one-time script. It bumps the workspace version + the internal dependency pins and cuts
# the CHANGELOG "Unreleased" section to the new version. The one-time crates.io bootstrap is a
# separate script: scripts/first-publish.sh.
#
# Usage: scripts/release.sh <patch|minor|major|X.Y.Z>
#   patch/minor/major  bump the current [workspace.package] version accordingly
#   X.Y.Z              set an explicit version
#
# All member crates inherit `version.workspace = true`, so only the root Cargo.toml is edited:
# the [workspace.package] version and the 10 internal `ch32rv-* = { path=..., version="X" }` pins.
set -euo pipefail

cd "$(dirname "$0")/.."

LEVEL="${1:-}"
if [ -z "$LEVEL" ]; then
  echo "usage: scripts/release.sh <patch|minor|major|X.Y.Z>" >&2
  exit 2
fi

# Current version = the `version = "..."` line at the start of a line (the [workspace.package] one;
# the dependency pins are `... version = "..."`, never at column 0).
CUR=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
if [ -z "$CUR" ]; then
  echo "error: could not find the [workspace.package] version in Cargo.toml" >&2
  exit 1
fi

case "$LEVEL" in
  major|minor|patch)
    IFS=. read -r MAJ MIN PAT <<<"$CUR"
    case "$LEVEL" in
      major) MAJ=$((MAJ + 1)); MIN=0; PAT=0 ;;
      minor) MIN=$((MIN + 1)); PAT=0 ;;
      patch) PAT=$((PAT + 1)) ;;
    esac
    NEW="$MAJ.$MIN.$PAT"
    ;;
  [0-9]*.[0-9]*.[0-9]*)
    NEW="$LEVEL"
    ;;
  *)
    echo "usage: scripts/release.sh <patch|minor|major|X.Y.Z>" >&2
    exit 2
    ;;
esac

echo "bumping $CUR -> $NEW"

# Bump every `version = "$CUR"` in the root manifest: the [workspace.package] version + the 10
# internal path-dep pins. External deps never equal an internal version, so this is a clean swap;
# the count guard catches it if that ever stops being true.
EXPECTED=$(grep -c "version = \"$CUR\"" Cargo.toml)
sed -i "s/version = \"$CUR\"/version = \"$NEW\"/g" Cargo.toml
GOT=$(grep -c "version = \"$NEW\"" Cargo.toml)
if [ "$GOT" -ne "$EXPECTED" ]; then
  echo "error: expected to rewrite $EXPECTED version pins, rewrote $GOT" >&2
  exit 1
fi

# Cut CHANGELOG: keep an empty "## Unreleased" on top, move its accumulated bullets under the new
# version header. Requires a "## Unreleased" line (GNU sed, first match only).
DATE=$(date +%F)
if ! grep -q '^## Unreleased$' CHANGELOG.md; then
  echo "error: CHANGELOG.md has no '## Unreleased' section to cut" >&2
  exit 1
fi
sed -i "0,/^## Unreleased$/s//## Unreleased\n\n## $NEW - $DATE/" CHANGELOG.md

# Refresh Cargo.lock's workspace-member versions (leaves external deps pinned) so the release
# commit includes an up-to-date lockfile (the binaries job builds with --locked).
cargo update --workspace >/dev/null

echo "$NEW"
