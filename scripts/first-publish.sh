#!/usr/bin/env bash
# first-publish.sh - ONE-TIME crates.io bootstrap. Run this exactly once, ever.
#
# The very first publish of each new crate needs an API token (crates.io has no pre-registration),
# so it cannot go through the token-free GitHub Actions release. This script claims the eight crate
# names in dependency order from your machine with a token. After it succeeds you register each
# crate's Trusted Publisher (printed at the end); every release after that runs token-free from
# .github/workflows/release.yml and you never run this script again.
#
# The recurring per-release version bump is a different script: scripts/release.sh.
#
# Prerequisites:
#   - `cargo login <token>` done, or CARGO_REGISTRY_TOKEN exported (a crates.io API token).
#   - You are on the exact commit you want as the first published version (run scripts/release.sh
#     first if you still need to set the version).
set -euo pipefail

cd "$(dirname "$0")/.."

# Dependency order: contract -> usb-wch-win -> usb/dmi/target -> wchlink/flash -> debug -> the CLI.
# usb-wch-win before usb: ch32rv-usb has a cfg(windows) dependency on it (docs/windows-wch-driver.ja.md).
CRATES=(contract usb-wch-win usb dmi target wchlink flash debug)

crate_exists() {
  # crates.io returns 200 for an existing crate, 404 otherwise. It requires a User-Agent.
  local code
  code=$(curl -s -o /dev/null -w '%{http_code}' \
    -H 'User-Agent: ch32rv-first-publish (https://github.com/ch32-riscv-ug/ch32rv)' \
    "https://crates.io/api/v1/crates/$1")
  [ "$code" = "200" ]
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

echo "== one-time crates.io bootstrap =="
for c in "${CRATES[@]}"; do
  publish_one "ch32rv-$c"
done
publish_one "ch32rv"

cat <<'EOF'

== done. Now register the Trusted Publisher for each crate (one time, web UI) ==
For every crate below, open its crates.io Settings -> Trusted Publishing -> Add, and enter:
  owner    : ch32-riscv-ug
  repo     : ch32rv
  workflow : release.yml
  (environment: leave empty unless you gate releases behind a GitHub Environment)

  ch32rv-contract  ch32rv-usb-wch-win  ch32rv-usb   ch32rv-dmi   ch32rv-target
  ch32rv-wchlink   ch32rv-flash        ch32rv-debug ch32rv

After that, all future releases run token-free from the Actions "Release" workflow.
EOF
