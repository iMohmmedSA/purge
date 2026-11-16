#!/bin/sh
# Installs or updates purge on Linux or macOS:
#
#   curl -LsSf https://github.com/iMohmmedSA/purge/releases/latest/download/install.sh | sh
#
# Downloads the release archive for this system, checks its SHA-256, and
# puts `purge`, `purge-rs` and `purged` into ~/.local/bin, adding that
# folder to your shell profile if it is not on your PATH. Run it again to
# update: if the background service is installed, it is restarted on the
# new version. A purge installed with `cargo install` is removed, so the
# two never compete.
#
# Optional settings:
#   PURGE_VERSION=v0.1.0      install this release instead of the latest
#   PURGE_INSTALL_DIR=<dir>   install here instead of ~/.local/bin
#   PURGE_FORCE=1             reinstall even if this version is installed
#   PURGE_NO_MODIFY_PATH=1    do not add the install folder to your shell
#                             profile
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

# The version of the purge installed in $INSTALL_DIR, like `0.1.0`.
installed_version() {
    [ -x "$INSTALL_DIR/purge" ] || return 0
    "$INSTALL_DIR/purge" --version 2> /dev/null | sed -n 's/^purge //p'
}

# Removes purge installed with `cargo install`, which could run instead of
# this one: the `purge` package, and `purge-cli` and `purge-daemon` from
# before the first release. Only packages Cargo itself lists are touched.
remove_cargo_installs() {
    command -v cargo > /dev/null 2>&1 || return 0
    installed=$(cargo install --list 2> /dev/null) || return 0
    for package in purge purge-cli purge-daemon; do
        printf '%s\n' "$installed" | grep -q "^$package v" || continue
        if cargo uninstall "$package" > /dev/null 2>&1; then
            say "removed the copy Cargo installed (\`$package\`)"
        else
            say "warning: could not remove the copy Cargo installed; run \`cargo uninstall $package\`"
        fi
    done
}

# Other purge installations on the PATH, which may run instead of this one.
# Only folders holding `purged` or `purge-rs` count: macOS has a `purge` of
# its own, which is never run or reported.
warn_other_copies() {
    old_ifs=$IFS
    IFS=:
    for dir in $PATH; do
        [ -n "$dir" ] && [ "$dir" != "$INSTALL_DIR" ] || continue
        if [ -x "$dir/purged" ] || [ -x "$dir/purge-rs" ]; then
            say "warning: another purge is installed in $dir and may run instead; remove it"
            say "  (\`sudo apt remove purge\` or \`sudo dnf remove purge\` if a package installed it)"
        fi
    done
    IFS=$old_ifs
}

# Marks the line the installer adds to a shell profile.
PROFILE_MARKER="# added by the purge installer"

# Adds $INSTALL_DIR to the PATH in the profile of the user's shell, once.
# On macOS it goes last, so macOS's own `purge` stays first.
add_to_path() {
    if [ -n "${PURGE_NO_MODIFY_PATH:-}" ]; then
        say "$INSTALL_DIR is not on your PATH; add it to use purge from any folder"
        return 0
    fi
    darwin=false
    [ "$(uname -s)" = Darwin ] && darwin=true
    case "${SHELL:-}" in
        */zsh) profile="$HOME/.zshrc" ;;
        */bash) if $darwin; then profile="$HOME/.bash_profile"; else profile="$HOME/.bashrc"; fi ;;
        */fish) profile="$HOME/.config/fish/conf.d/purge.fish" ;;
        *) profile="$HOME/.profile" ;;
    esac
    case "$profile" in
        *.fish)
            if $darwin; then flag=" --append"; else flag=""; fi
            line="fish_add_path$flag \"$INSTALL_DIR\""
            ;;
        *)
            if $darwin; then
                line="export PATH=\"\$PATH:$INSTALL_DIR\""
            else
                line="export PATH=\"$INSTALL_DIR:\$PATH\""
            fi
            ;;
    esac
    if [ -f "$profile" ] && grep -qF "$PROFILE_MARKER" "$profile"; then
        say "$INSTALL_DIR is added to your PATH in $profile; open a new terminal to use it"
        return 0
    fi
    mkdir -p "$(dirname "$profile")"
    printf '\n%s\n%s\n' "$PROFILE_MARKER" "$line" >> "$profile"
    say "added $INSTALL_DIR to your PATH in $profile; open a new terminal to use it"
}

main() {
    need curl
    need tar
    command -v sha256sum > /dev/null 2>&1 || need shasum

    target=$(target)
    version="${PURGE_VERSION:-$(latest_version)}"
    name="purge-$version-$target"
    previous=$(installed_version)
    if [ "v$previous" = "$version" ] && [ -z "${PURGE_FORCE:-}" ]; then
        say "purge $previous is already installed and up to date"
        exit 0
    fi
    if [ "v$previous" = "$version" ]; then
        say "reinstalling purge $previous in $INSTALL_DIR"
    elif [ -n "$previous" ]; then
        say "updating purge $previous to ${version#v} in $INSTALL_DIR"
    else
        say "installing purge ${version#v} for $target into $INSTALL_DIR"
    fi

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
    if [ -n "$previous" ] && [ "v$previous" != "$version" ]; then
        say "updated purge $previous to ${version#v}"
    else
        say "installed purge, purge-rs and purged"
    fi

    remove_cargo_installs

    # A running service keeps the old version until it restarts; installing
    # it again points it at these files and restarts it.
    status=$("$INSTALL_DIR/purge" service status 2> /dev/null || true)
    if [ -n "$status" ] && [ "$status" != "not installed" ]; then
        if "$INSTALL_DIR/purge" service install > /dev/null 2>&1; then
            say "restarted the background service on ${version#v}"
        else
            say "warning: could not restart the background service; run \`purge service install\`"
        fi
    fi
    warn_other_copies

    # macOS has a `purge` of its own: there, use `purge-rs`, and add the
    # folder after everything else so Apple's command stays as it is.
    if [ "$(uname -s)" = Darwin ]; then
        command=purge-rs
    else
        command=purge
    fi
    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *) add_to_path ;;
    esac
    [ "$command" = purge ] || say "on macOS, use \`purge-rs\`: macOS has a \`purge\` of its own"
    [ -n "$previous" ] || say "get started: $command config init ~/Dev"
}

main "$@"
