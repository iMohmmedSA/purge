#!/usr/bin/env bash
# Publishes every crate of the workspace to crates.io, one at a time with
# dependencies first. Crates whose version is already on crates.io are
# skipped, so a run that stopped half way can simply be started again.
# crates.io lets an account publish only a few brand-new crates in a burst,
# then about one every ten minutes; when it says to slow down, wait and retry.
#
# DRY_RUN=1 prints what would be published without publishing.
set -euo pipefail

# Workspace crates in dependency order: each after everything it needs.
order=$(cargo metadata --format-version 1 --locked | python3 -c '
import json, sys
meta = json.load(sys.stdin)
members = set(meta["workspace_members"])
names = {p["id"]: p["name"] for p in meta["packages"] if p["id"] in members}
needs = {node["id"]: [d["pkg"] for d in node["deps"] if d["pkg"] in members]
         for node in meta["resolve"]["nodes"] if node["id"] in members}
done, order = set(), []
def visit(pkg):
    if pkg in done:
        return
    done.add(pkg)
    for dep in sorted(needs[pkg], key=names.get):
        visit(dep)
    order.append(names[pkg])
for pkg in sorted(names, key=names.get):
    visit(pkg)
print(" ".join(order))
')

version=$(cargo metadata --format-version 1 --no-deps --locked \
    | python3 -c 'import json,sys; print({p["name"]: p["version"] for p in json.load(sys.stdin)["packages"]}["purge"])')

published() {
    curl -fsS -o /dev/null -A "purge-release-script" \
        "https://crates.io/api/v1/crates/$1/$version" 2> /dev/null
}

echo "publishing $version: $order"
for crate in $order; do
    if published "$crate"; then
        echo "skip     $crate $version (already on crates.io)"
        continue
    fi
    if [ -n "${DRY_RUN:-}" ]; then
        echo "publish  $crate $version (dry run)"
        continue
    fi
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        if output=$(cargo publish --package "$crate" --locked 2>&1); then
            echo "done     $crate $version"
            break
        fi
        echo "$output" >&2
        if [ "$attempt" -lt 10 ] && grep -qiE "429|too many|rate limit" <<< "$output"; then
            echo "wait     crates.io rate limit; retrying $crate in 11 minutes" >&2
            sleep 660
        else
            exit 1
        fi
    done
done
