#!/bin/sh
#
# Install kertweaker.
#
# The script gurt fetches when someone runs
#
#   gurt install kertweaker
#
# It also works on its own:
#
#   curl -fsSL https://raw.githubusercontent.com/clrealy/kertweaker/main/packaging/gurt/install.sh -o install-kertweaker.sh
#   sh install-kertweaker.sh
#
# What it does:
#
#   1. Checks Rust is present (so the message is clear if it is not).
#   2. Clones kertweaker to $KERTWEAKER_SRC (default: ~/.local/share/kertweaker/src).
#   3. Builds the release binary.
#   4. Installs it to $PREFIX/bin/kertweaker (default: /usr/local/bin).
#
# Nothing else is done - no shell files are edited, no services are enabled,
# no root is needed past the install step itself. To uninstall, delete the
# binary and the source directory.

set -eu

: "${PREFIX:=/usr/local}"
: "${KERTWEAKER_SRC:=${XDG_DATA_HOME:-$HOME/.local/share}/kertweaker/src}"
: "${KERTWEAKER_REPO:=https://github.com/clrealy/kertweaker.git}"
: "${KERTWEAKER_REF:=main}"

say() { printf '[kertweaker] %s\n' "$*"; }
die() { printf '[kertweaker] %s\n' "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

have git || die "git is required (install git, then re-run this script)."
have cargo || die "Rust's cargo is required (https://rustup.rs), then re-run this script."

# ------------------------------------------------------------------- clone
if [ -d "$KERTWEAKER_SRC/.git" ]; then
    say "Updating existing clone at $KERTWEAKER_SRC"
    git -C "$KERTWEAKER_SRC" fetch --quiet origin "$KERTWEAKER_REF"
    git -C "$KERTWEAKER_SRC" checkout --quiet "$KERTWEAKER_REF"
    git -C "$KERTWEAKER_SRC" reset --hard --quiet "origin/$KERTWEAKER_REF" 2>/dev/null \
        || git -C "$KERTWEAKER_SRC" reset --hard --quiet "$KERTWEAKER_REF"
else
    say "Cloning $KERTWEAKER_REPO to $KERTWEAKER_SRC"
    mkdir -p "$(dirname "$KERTWEAKER_SRC")"
    git clone --quiet --depth 50 --branch "$KERTWEAKER_REF" "$KERTWEAKER_REPO" "$KERTWEAKER_SRC"
fi

# ------------------------------------------------------------------- build
say "Building kertweaker (this takes a minute or two)"
(cd "$KERTWEAKER_SRC" && cargo build --release --quiet)

built="$KERTWEAKER_SRC/target/release/kertweaker"
[ -x "$built" ] || die "Build finished but $built is missing."

# ------------------------------------------------------------------ install
target="$PREFIX/bin/kertweaker"
if [ -w "$PREFIX/bin" ] || [ "$(id -u)" = 0 ]; then
    install -m 0755 "$built" "$target"
else
    say "Installing to $target (needs sudo)"
    sudo install -m 0755 "$built" "$target"
fi

say "Installed $("$target" --version)"
say "Start the GUI with:   kertweaker"
say "Check the machine:    kertweaker doctor"
