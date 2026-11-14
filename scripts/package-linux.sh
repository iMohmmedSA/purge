#!/usr/bin/env bash
# Builds the .deb and .rpm packages for one Linux target from release
# binaries already built with `cargo build --release --target <target>`,
# and prints their paths. Needs cargo-deb and cargo-generate-rpm.
set -euo pipefail

target="${1:?usage: scripts/package-linux.sh <target-triple>}"

cargo deb --package purge --target "$target" --no-build >&2
cargo generate-rpm --package apps/cli --target "$target" >&2

ls target/"$target"/debian/*.deb target/"$target"/generate-rpm/*.rpm
