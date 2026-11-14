#!/bin/sh
# Installs purge on Linux or macOS:
#
#   curl -LsSf https://github.com/iMohmmedSA/purge/releases/latest/download/install.sh | sh
#
# Downloads the release archive for this system, checks its SHA-256, and
# puts `purge`, `purge-rs` and `purged` into ~/.local/bin.
#
# Optional settings:
#   PURGE_VERSION=v0.1.0      install this release instead of the latest
#   PURGE_INSTALL_DIR=<dir>   install here instead of ~/.local/bin
set -eu

REPO="iMohmmedSA/purge"
DOWNLOADS="${PURGE_DOWNLOAD_URL:-https://github.com/$REPO/releases}"
INSTALL_DIR="${PURGE_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'purge: %s\n' "$*"; }
fail() { printf 'purge: error: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" > /dev/null 2>&1 || fail "this installer needs \`$1\`"
}

target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux) os=unknown-linux-gnu ;;
        Darwin) os=apple-darwin ;;
        *) fail "no prebuilt binaries for $os; install with \`cargo install --locked purge\`" ;;
    esac
    case "$arch" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) fail "no prebuilt binaries for $arch; install with \`cargo install --locked purge\`" ;;
    esac
    # Under Rosetta, an Apple Silicon Mac reports x86_64; prefer the native build.
    if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] \
        && [ "$(sysctl -n sysctl.proc_translated 2> /dev/null || true)" = 1 ]; then
        arch=aarch64
    fi
    echo "$arch-$os"
}

# The tag of the newest release, from where GitHub's "latest" link points.
latest_version() {
    url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$DOWNLOADS/latest") \
        || fail "cannot reach GitHub"
    version=${url##*/}
    case "$version" in
        v*) echo "$version" ;;
        *) fail "no release found at $DOWNLOADS" ;;
    esac
}

sha256() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    else
        shasum -a 256 "$1" | cut -d ' ' -f 1
    fi
}

main() {
    need curl
    need tar
    command -v sha256sum > /dev/null 2>&1 || need shasum

    target=$(target)
    version="${PURGE_VERSION:-$(latest_version)}"
    name="purge-$version-$target"
    say "installing $version for $target into $INSTALL_DIR"

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    curl -fsSL -o "$tmp/$name.tar.gz" "$DOWNLOADS/download/$version/$name.tar.gz" \
        || fail "cannot download $name.tar.gz"
    curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$DOWNLOADS/download/$version/$name.tar.gz.sha256" \
        || fail "cannot download the checksum for $name.tar.gz"

    expected=$(cut -d ' ' -f 1 < "$tmp/$name.tar.gz.sha256")
    actual=$(sha256 "$tmp/$name.tar.gz")
    [ "$expected" = "$actual" ] || fail "checksum mismatch for $name.tar.gz; nothing was installed"

    tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
    mkdir -p "$INSTALL_DIR"
    for bin in purge purge-rs purged; do
        install -m 755 "$tmp/$name/$bin" "$INSTALL_DIR/$bin"
    done
    say "installed purge, purge-rs and purged"

    case ":$PATH:" in
        *":$INSTALL_DIR:"*) say "get started: purge config init ~/Dev" ;;
        *)
            say "$INSTALL_DIR is not on your PATH; add this line to your shell profile:"
            printf '\n    export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
            ;;
    esac
}

main "$@"
