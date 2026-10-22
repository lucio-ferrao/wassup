#!/bin/sh
# Installs or updates wa, the wassup client, for the current user only, nothing as root:
#
#   curl -fsSL https://raw.githubusercontent.com/lucio-ferrao/wassup/main/install.sh | sh
#
# On Linux, x86_64 or 64-bit ARM, it fetches the program already built from the latest release (static, it runs on
# any distribution) into ~/.local/bin, or WA_BIN_DIR. Elsewhere, when there's no release to fetch, or with
# WA_FROM_SOURCE=1, it builds it instead: Rust from rustup into the home folder when cargo isn't there, then wa with
# cargo install, into cargo's bin folder; only a C compiler has to be there already.
set -eu

REPO=https://github.com/lucio-ferrao/wassup
USER_PATH=$PATH

say() { printf 'wassup: %s\n' "$*"; }
fail() { say "$*" >&2; exit 1; }

# Says how to put `dir` on the PATH when it isn't, and which wa runs first when another comes before it.
check_path() {
    dir=$1
    case ":$USER_PATH:" in
        *":$dir:"*)
            first=$(PATH=$USER_PATH command -v wa 2>/dev/null || true)
            if [ -n "$first" ] && [ "$first" != "$dir/wa" ]; then
                say "another wa comes first on your PATH, $first; remove it or put $dir before it"
            fi
            ;;
        *)
            say "add $dir to your PATH to run it as wa, for example in ~/.profile:"
            printf '  export PATH="%s:$PATH"\n' "$dir"
            ;;
    esac
}

# The program from the latest release, for this machine; false when there's none to fetch.
fetch_binary() {
    [ "$(uname -s)" = Linux ] || return 1
    case $(uname -m) in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) return 1 ;;
    esac
    command -v curl >/dev/null 2>&1 || return 1
    dir=${WA_BIN_DIR:-$HOME/.local/bin}
    mkdir -p "$dir" || fail "can't make $dir"
    # The download goes beside the program, not to /tmp, which may be small or full.
    tmp=$(mktemp -d "$dir/.wa.XXXXXX") || fail "can't write in $dir"
    trap 'rm -rf "$tmp"' EXIT
    say "fetching wa for $arch Linux"
    curl -fsSL "$REPO/releases/latest/download/wa-$arch-linux.tar.gz" -o "$tmp/wa.tar.gz" || return 1
    tar -xzf "$tmp/wa.tar.gz" -C "$tmp" wa || return 1
    # Moved into place in one step, so a wa that's running keeps its own file.
    chmod 755 "$tmp/wa" && mv -f "$tmp/wa" "$dir/wa" || fail "can't write $dir/wa"
    say "wa installed at $dir/wa"
    check_path "$dir"
}

# The program built here with cargo install.
build() {
    CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
    if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
        fail "no C compiler (cc, gcc or clang); install one first, as root: build-essential on Debian or Ubuntu, base-devel on Arch, gcc on Fedora, the Xcode command line tools on macOS"
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        if [ -x "$CARGO_HOME/bin/cargo" ]; then
            PATH="$CARGO_HOME/bin:$PATH"
        else
            command -v curl >/dev/null 2>&1 || fail "curl is needed to fetch Rust"
            say "installing Rust with rustup, into $CARGO_HOME"
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal
            PATH="$CARGO_HOME/bin:$PATH"
        fi
    fi
    # wa takes Rust 1.88 or later (if-let chains); a distribution's cargo may be older.
    version=$(rustc --version 2>/dev/null | sed -n 's/^rustc \([0-9]*\)\.\([0-9]*\).*/\1 \2/p')
    set -- ${version:-0 0}
    if [ "$1" -eq 1 ] && [ "$2" -lt 88 ]; then
        if command -v rustup >/dev/null 2>&1; then
            say "updating Rust with rustup"
            rustup update stable
        else
            fail "Rust $1.$2 is too old, 1.88 or later is needed; remove the distribution's and run this again to get it from rustup"
        fi
    fi
    say "building wa (a few minutes the first time)"
    cargo install --git "$REPO" --locked --force
    say "wa installed at $CARGO_HOME/bin/wa"
    check_path "$CARGO_HOME/bin"
}

if [ "${WA_FROM_SOURCE:-0}" != 1 ] && fetch_binary; then
    exit 0
fi
[ "${WA_FROM_SOURCE:-0}" = 1 ] || say "no program to fetch for this machine, building it instead"
build
