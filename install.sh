#!/bin/sh
# Installs or updates wa, the wassup client, for the current user only, nothing as root:
#
#   curl -fsSL https://raw.githubusercontent.com/lucio-ferrao/wassup/main/install.sh | sh
#
# Rust comes from rustup into the home folder when cargo isn't there; wa is then built from the repository with
# cargo install, into cargo's bin folder. Only a C compiler has to be there already (for the cryptography library
# and SQLite, which are built with it).
set -eu

REPO=https://github.com/lucio-ferrao/wassup
CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
USER_PATH=$PATH

say() { printf 'wassup: %s\n' "$*"; }
fail() { say "$*" >&2; exit 1; }

if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
    fail "no C compiler (cc, gcc or clang); install one first, as root: build-essential on Debian or Ubuntu, base-devel on Arch, gcc on Fedora"
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

BIN="$CARGO_HOME/bin/wa"
say "wa installed at $BIN"
case ":$USER_PATH:" in
    *":$CARGO_HOME/bin:"*) exit 0 ;;
esac
say "add $CARGO_HOME/bin to your PATH to run it as wa, for example in ~/.profile:"
printf '  export PATH="%s/bin:$PATH"\n' "$CARGO_HOME"
