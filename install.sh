#!/usr/bin/env bash
# rustterm installer — curl -fsSL .../install.sh | bash
#
# Downloads the latest GitHub release binary for your platform into
# $PREFIX (default ~/.local/bin). When no prebuilt release matches,
# falls back to `cargo install --git` if a Rust toolchain is present.
set -euo pipefail

REPO="mahadi-jsdev/rustterm"
BIN="rustterm"
PREFIX="${PREFIX:-$HOME/.local/bin}"

say() { printf 'rustterm: %s\n' "$*"; }
die() { say "error: $*" >&2; exit 1; }

detect_target() {
    local os arch
    os="$(uname -s)"; arch="$(uname -m)"
    case "$os" in
        Linux)  os="linux" ;;
        Darwin) os="macos" ;;
        *)      return 1 ;;
    esac
    case "$arch" in
        x86_64|amd64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *)             return 1 ;;
    esac
    printf '%s-%s' "$os" "$arch"
}

install_release() {
    local target url tmp
    target="$1"
    url="https://github.com/$REPO/releases/latest/download/$BIN-$target.tar.gz"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    say "downloading $url"
    if ! curl -fsSL "$url" -o "$tmp/$BIN.tar.gz"; then
        rm -rf "$tmp"; trap - EXIT
        return 1
    fi
    tar -xzf "$tmp/$BIN.tar.gz" -C "$tmp"
    # Binary may sit at the tarball root or inside a target dir.
    local bin
    bin="$(find "$tmp" -name "$BIN" -type f | head -1)"
    if [ -z "$bin" ]; then
        rm -rf "$tmp"; trap - EXIT
        return 1
    fi
    mkdir -p "$PREFIX"
    install -m 0755 "$bin" "$PREFIX/$BIN" || { rm -rf "$tmp"; trap - EXIT; return 1; }
    rm -rf "$tmp"; trap - EXIT
    say "installed $PREFIX/$BIN"
    case ":$PATH:" in
        *":$PREFIX:"*) ;;
        *) say "note: $PREFIX is not on your PATH" ;;
    esac
}

main() {
    local target=""
    if target="$(detect_target)" && install_release "$target"; then
        :
    elif command -v cargo >/dev/null 2>&1; then
        say "no prebuilt binary for this platform — building from source"
        cargo install --git "https://github.com/$REPO" --locked 2>/dev/null \
            || cargo install --git "https://github.com/$REPO"
    else
        die "no prebuilt binary for $(uname -s)/$(uname -m) and no cargo found.
       install Rust (https://rustup.rs) and rerun, or build from source."
    fi
    say "done — run: $BIN"
}

main "$@"
