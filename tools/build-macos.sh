#!/usr/bin/env bash
#
# Builds InputShare.app and packages it as a .dmg. Run it on the Mac.
#
#     tools/build-macos.sh              # for this Mac's processor
#     tools/build-macos.sh --universal  # one build for Apple Silicon and Intel
#
# There is no cross-compiling from Windows or Linux: a macOS app has to be
# linked against Apple's SDK and signed with Apple's tools, both of which only
# exist on a Mac.
#
# The result is ad-hoc signed, not signed with a Developer ID. That is enough for
# the machine that built it and for any Mac the file is copied to by hand, but
# Gatekeeper will still ask the first time. The alternative is a paid Apple
# developer account.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="InputShare"
VOLUME="InputShare"

say() { printf '\033[1m%s\033[0m\n' "$*"; }
die() {
    printf '\033[31m%s\033[0m\n' "$*" >&2
    exit 1
}

# ------------------------------------------------------------- prerequisites

[ "$(uname -s)" = "Darwin" ] || die "This has to run on a Mac; it builds a Mac application."
command -v cargo >/dev/null || die "Rust is not installed. See https://rustup.rs"
xcode-select --print-path >/dev/null 2>&1 ||
    die "Apple's command line tools are missing. Run: xcode-select --install"

if ! cargo tauri --version >/dev/null 2>&1; then
    say "Installing the Tauri CLI (once, a few minutes)…"
    cargo install tauri-cli --version "^2" --locked
fi

UNIVERSAL=0
[ "${1:-}" = "--universal" ] && UNIVERSAL=1

TARGET_ARGS=()
BUNDLE_DIR="$ROOT/target/release/bundle/macos"
if [ "$UNIVERSAL" = "1" ]; then
    say "Adding both processor targets…"
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    TARGET_ARGS=(--target universal-apple-darwin)
    BUNDLE_DIR="$ROOT/target/universal-apple-darwin/release/bundle/macos"
fi

# -------------------------------------------------------------------- build

say "Building $APP_NAME…"
(cd "$ROOT/crates/is-ui" && cargo tauri build --bundles app "${TARGET_ARGS[@]}")

APP="$BUNDLE_DIR/$APP_NAME.app"
[ -d "$APP" ] || die "The build finished but $APP is not there."

# Ad-hoc signing, which matters more here than it looks. macOS remembers
# Accessibility and Input Monitoring permission against the app's signature; an
# unsigned app gets a new identity on every rebuild, so the permission the user
# granted yesterday silently stops applying.
say "Signing (ad-hoc)…"
codesign --force --deep --sign - "$APP"
codesign --verify --deep "$APP" || die "The signature did not verify."

# ------------------------------------------------------------------ package

VERSION="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$ROOT/crates/is-ui/tauri.conf.json" | head -n 1)"
DMG="$ROOT/target/${APP_NAME}_${VERSION:-dev}.dmg"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

cp -R "$APP" "$STAGE/"
# The usual drag-to-install window: the app on one side, Applications on the
# other.
ln -s /Applications "$STAGE/Applications"

say "Packaging $(basename "$DMG")…"
rm -f "$DMG"
hdiutil create -volname "$VOLUME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null

say ""
say "Done: $DMG"
cat <<'NOTES'

Installing it
  Open the .dmg and drag InputShare to Applications.
  The first launch: right-click the app and choose Open, because it is not
  signed with a paid Developer ID and a double-click will be refused.

Two permissions, both required, neither of which the app can grant itself
  System Settings > Privacy & Security > Accessibility      -> InputShare
  System Settings > Privacy & Security > Input Monitoring    -> InputShare
  Without Accessibility the app cannot read or take over the keyboard and
  mouse, and says so. Grant them, then quit and reopen the app.

NOTES
