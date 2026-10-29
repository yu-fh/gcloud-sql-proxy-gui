#!/usr/bin/env bash
# Build "Cloud SQL Proxy.app" and a DMG around it.
#
#   scripts/bundle.sh            universal (arm64 + x86_64) -- what a release ships
#   scripts/bundle.sh --native   this machine's architecture only, for a quick local build
#
# Output goes to dist/. Signing is ad-hoc unless SIGN_IDENTITY names a
# Developer ID identity, e.g.
#   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" scripts/bundle.sh
# Ad-hoc is not optional: lipo invalidates the linker's signature, and an
# unsigned fat binary will not launch on Apple Silicon.
set -euo pipefail

cd "$(dirname "$0")/.."

NAME="Cloud SQL Proxy"
BIN="fh-cloud-sql-proxy-gui"
IDENTIFIER="ai.firsthand.fh-cloud-sql-proxy-gui"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[ -n "$VERSION" ] || { echo "could not read the version from Cargo.toml" >&2; exit 1; }

native=false
[ "${1:-}" = "--native" ] && native=true

OUT="dist"
APP="$OUT/$NAME.app"
rm -rf "$OUT"
mkdir -p "$OUT"

if $native; then
  cargo build --release
  EXE="target/release/$BIN"
  SUFFIX="$(uname -m | sed 's/arm64/aarch64/')"
else
  cargo build --release --target aarch64-apple-darwin
  cargo build --release --target x86_64-apple-darwin
  EXE="$OUT/$BIN"
  lipo -create \
    "target/aarch64-apple-darwin/release/$BIN" \
    "target/x86_64-apple-darwin/release/$BIN" \
    -output "$EXE"
  SUFFIX="universal"
fi

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$EXE" "$APP/Contents/MacOS/$BIN"
cp icons/icon.icns "$APP/Contents/Resources/icon.icns"
sed "s/@VERSION@/$VERSION/g" packaging/Info.plist > "$APP/Contents/Info.plist"
$native || rm "$EXE"

codesign --force --sign "${SIGN_IDENTITY:--}" --identifier "$IDENTIFIER" \
  ${SIGN_IDENTITY:+--options runtime --timestamp} "$APP"
codesign --verify --strict "$APP"

# The DMG holds the app and an Applications shortcut to drag it onto.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
DMG="$OUT/Cloud-SQL-Proxy_${VERSION}_${SUFFIX}.dmg"
hdiutil create -volname "$NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null

echo "app: $APP"
echo "dmg: $DMG"
