#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

./tools/selftest.py

if [[ ! -f Cargo.lock ]]; then
  echo "Generating Cargo.lock for the application build..."
  cargo generate-lockfile
fi

echo "Building optimized release binary..."
cargo build --release --locked

VERSION="$(python3 - <<'PY'
from pathlib import Path
import re
text = Path('Cargo.toml').read_text()
m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
print(m.group(1))
PY
)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BINARY="$TARGET_DIR/release/agents-usage"
ARCH="$(uname -m)"
DIST="$ROOT/dist"
BUNDLE="$DIST/agents-usage-$VERSION-linux-$ARCH"

rm -rf "$DIST"
mkdir -p "$BUNDLE/bin" \
         "$BUNDLE/share/applications" \
         "$BUNDLE/share/icons/hicolor/scalable/apps" \
         "$BUNDLE/share/gnome-shell/extensions" \
         "$BUNDLE/share/doc/agents-usage"

install -m 0755 "$BINARY" "$BUNDLE/bin/agents-usage"
cp "$ROOT/packaging/linux/agents-usage.desktop.in" "$BUNDLE/share/applications/agents-usage.desktop.in"
cp "$ROOT/packaging/linux/agents-usage-autostart.desktop.in" "$BUNDLE/share/applications/agents-usage-autostart.desktop.in"
cp "$ROOT/packaging/linux/agents-usage.svg" "$BUNDLE/share/icons/hicolor/scalable/apps/agents-usage.svg"
cp -a "$ROOT/integration/gnome-shell/extension" "$BUNDLE/share/gnome-shell/extensions/agents-usage@local"
cp "$ROOT/README.md" "$ROOT/LICENSE" "$ROOT/THIRD_PARTY_ASSETS.md" "$BUNDLE/share/doc/agents-usage/"
cp -a "$ROOT/third_party" "$BUNDLE/share/doc/agents-usage/third_party"

(
  cd "$DIST"
  tar -czf "agents-usage-$VERSION-linux-$ARCH.tar.gz" "$(basename "$BUNDLE")"
)

printf '\nRelease build complete.\n'
printf 'Binary: %s\n' "$BUNDLE/bin/agents-usage"
printf 'Bundle: %s\n' "$DIST/agents-usage-$VERSION-linux-$ARCH.tar.gz"
printf '\nInstall for this user with:\n  ./tools/install-user.sh\n'
