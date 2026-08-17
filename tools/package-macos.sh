#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
BINARY="${1:-$ROOT/target/release/agents-usage}"
DIST="$ROOT/dist/packages"
WORK="$ROOT/dist/.macos-package-work"
APP="$WORK/Agents Usage.app"

[[ "$(uname -s)" == Darwin ]] || { echo "macOS packaging must run on macOS." >&2; exit 1; }
test -x "$BINARY" || { echo "Missing executable: $BINARY" >&2; exit 1; }
rm -rf "$WORK"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$DIST"
install -m 0755 "$BINARY" "$APP/Contents/MacOS/agents-usage"
sed "s/@VERSION@/$VERSION/g" "$ROOT/packaging/macos/Info.plist.in" > "$APP/Contents/Info.plist"
install -m 0644 "$ROOT/packaging/linux/agents-usage.svg" "$APP/Contents/Resources/agents-usage.svg"
install -m 0644 "$ROOT/LICENSE" "$APP/Contents/Resources/LICENSE"
plutil -lint "$APP/Contents/Info.plist"
codesign --force --sign - --timestamp=none "$APP"

ditto -c -k --sequesterRsrc --keepParent "$APP" "$DIST/Agents_Usage-$VERSION-macos-universal.zip"
hdiutil create -quiet -volname "Agents Usage" -srcfolder "$APP" -ov -format UDZO \
  "$DIST/Agents_Usage-$VERSION-macos-universal.dmg"
codesign --verify --deep --strict "$APP"
hdiutil verify -quiet "$DIST/Agents_Usage-$VERSION-macos-universal.dmg"
unzip -tq "$DIST/Agents_Usage-$VERSION-macos-universal.zip"
echo "macOS ZIP and DMG are ready in $DIST (ad-hoc signed, not notarized)."
