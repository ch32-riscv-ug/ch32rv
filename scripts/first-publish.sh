#!/usr/bin/env bash
# first-publish.sh - crates.io bootstrap for newly added workspace crates.
#
# The very first publish of each new crate needs an API token (crates.io has no pre-registration),
# so it cannot go through the token-free GitHub Actions release. This script claims crate
# names in dependency order from your machine with a token. After it succeeds you register each
# crate's Trusted Publisher (printed at the end); every release after that runs token-free from
# .github/workflows/release.yml and you never run this script again.
#
# The recurring per-release version bump is a different script: scripts/release.sh.
#
# Prerequisites:
#   - `cargo login <token>` done, or CARGO_REGISTRY_TOKEN exported (a crates.io API token).
#   - Run this from a clean checkout of main.
set -euo pipefail

cd "$(dirname "$0")/.."

if [ -n "$(git status --porcelain)" ]; then
  echo "error: first-publish must run from a clean checkout" >&2
  exit 1
fi

# Dependency order: contract -> usb-wch-win -> usb/dmi/target -> wchlink/flash -> debug -> CLI.
# usb-wch-win before usb: ch32rv-usb has a cfg(windows) dependency on it (docs/windows-wch-driver.ja.md).
CRATES=(contract usb-wch-win usb dmi target wchlink flash debug)

# ch32rv-boot is new after 0.8.0, but current main is still versioned 0.8.0 and its capture/replay
# support uses ch32rv-usb APIs that are not present in the already-published ch32rv-usb 0.8.0.
# Claiming the name from current main would therefore fail package verification. Publish the
# capture-free HID implementation from the 0.8.0-compatible source commit once; the Release
# workflow can then publish the complete 0.9.0 crate through Trusted Publishing.
BOOTSTRAP_BOOT_REV=fad0881
bootstrap_root=""
bootstrap_worktree=""

cleanup_bootstrap_worktree() {
  if [ -n "$bootstrap_worktree" ] && [ -d "$bootstrap_worktree" ]; then
    git worktree remove --force "$bootstrap_worktree" >/dev/null 2>&1 || true
  fi
  if [ -n "$bootstrap_root" ] && [ -d "$bootstrap_root" ]; then
    rmdir "$bootstrap_root" >/dev/null 2>&1 || true
  fi
}
trap cleanup_bootstrap_worktree EXIT

crate_exists() {
  # crates.io returns 200 for an existing crate, 404 otherwise. It requires a User-Agent.
  local code
  code=$(curl -s -o /dev/null -w '%{http_code}' \
    -H 'User-Agent: ch32rv-first-publish (https://github.com/ch32-riscv-ug/ch32rv)' \
    "https://crates.io/api/v1/crates/$1")
  case "$code" in
    200) return 0 ;;
    404) return 1 ;;
    *)
      echo "error: crates.io lookup for $1 returned HTTP $code" >&2
      exit 1
      ;;
  esac
}

publish_one() {
  local pkg="$1"
  if crate_exists "$pkg"; then
    echo "skip  $pkg (already on crates.io - name already claimed)"
  else
    echo "publish $pkg"
    cargo publish -p "$pkg" # cargo waits for the index before the next dependent builds
  fi
}

publish_boot() {
  if crate_exists ch32rv-boot; then
    echo "skip  ch32rv-boot (already on crates.io - name already claimed)"
    return
  fi

  echo "publish ch32rv-boot (0.8.0-compatible bootstrap from $BOOTSTRAP_BOOT_REV)"
  git rev-parse --verify "$BOOTSTRAP_BOOT_REV^{commit}" >/dev/null
  bootstrap_root=$(mktemp -d)
  bootstrap_worktree="$bootstrap_root/ch32rv"
  git worktree add --detach "$bootstrap_worktree" "$BOOTSTRAP_BOOT_REV"
  sed -i 's/^publish = false$/publish = true/' "$bootstrap_worktree/crates/boot/Cargo.toml"
  grep -q '^publish = true$' "$bootstrap_worktree/crates/boot/Cargo.toml"
  (
    cd "$bootstrap_worktree"
    cargo publish --allow-dirty -p ch32rv-boot
  )
  cleanup_bootstrap_worktree
  bootstrap_root=""
  bootstrap_worktree=""
}

echo "== one-time crates.io bootstrap =="
for c in "${CRATES[@]}"; do
  publish_one "ch32rv-$c"
done
publish_boot
publish_one "ch32rv"

cat <<'EOF'

== done. Now register the Trusted Publisher for each crate published above (one time, web UI) ==
For this release, the only new crate should be ch32rv-boot. Open its crates.io
Settings -> Trusted Publishing -> Add, and enter:
  owner    : ch32-riscv-ug
  repo     : ch32rv
  workflow : release.yml
  (environment: leave empty unless you gate releases behind a GitHub Environment)

After that, all future releases run token-free from the Actions "Release" workflow.
EOF
