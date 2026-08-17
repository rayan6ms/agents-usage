#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
AUTOSTART=false
if [[ "${1:-}" == "--autostart" ]]; then AUTOSTART=true; fi

VERSION="$(python3 - <<'PY'
from pathlib import Path
import re
text = Path('Cargo.toml').read_text()
m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
print(m.group(1))
PY
)"
ARCH="$(uname -m)"
BUNDLE="$ROOT/dist/agents-usage-$VERSION-linux-$ARCH"
if [[ ! -x "$BUNDLE/bin/agents-usage" ]]; then
  echo "Release bundle not found. Building it first..."
  "$ROOT/tools/build-release.sh"
fi

BIN_DIR="$HOME/.local/bin"
APP_DIR="$HOME/.local/share/applications"
ICON_DIR="$HOME/.local/share/icons/hicolor/scalable/apps"
EXT_ROOT="$HOME/.local/share/gnome-shell/extensions"
EXT_UUID="agents-usage@local"
AUTOSTART_DIR="$HOME/.config/autostart"

mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR" "$EXT_ROOT"
install -m 0755 "$BUNDLE/bin/agents-usage" "$BIN_DIR/agents-usage"
install -m 0644 "$BUNDLE/share/icons/hicolor/scalable/apps/agents-usage.svg" "$ICON_DIR/agents-usage.svg"

sed "s|@EXEC@|$BIN_DIR/agents-usage|g" \
  "$BUNDLE/share/applications/agents-usage.desktop.in" \
  > "$APP_DIR/agents-usage.desktop"
chmod 0644 "$APP_DIR/agents-usage.desktop"

# Do not mutate live Shell extension state. Old prototype UUIDs are only
# reported; the user may remove them after confirming they are no longer used.
for OLD_UUID in \
  'agents-usage-tray-spike-r10@local' \
  'codex-usage-tray-spike-r9@local' \
  'codex-usage-tray-spike-r8@local' \
  'codex-usage-tray-spike@local'; do
  if [[ -d "$EXT_ROOT/$OLD_UUID" ]]; then
    echo "Old prototype extension left untouched: $EXT_ROOT/$OLD_UUID"
  fi
done

EXT_SOURCE="$BUNDLE/share/gnome-shell/extensions/$EXT_UUID"
EXT_TARGET="$EXT_ROOT/$EXT_UUID"
SOURCE_VERSION="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$EXT_SOURCE/metadata.json" | head -n 1)"
TARGET_VERSION=""
if [[ -f "$EXT_TARGET/metadata.json" ]]; then
  TARGET_VERSION="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$EXT_TARGET/metadata.json" | head -n 1)"
fi

# Never unload a running Shell extension merely for an application-binary
# update. When the extension itself changes, replace its on-disk directory
# atomically and let the next Shell session load the new version.
EXTENSION_CHANGED=false
if [[ "$SOURCE_VERSION" != "$TARGET_VERSION" ]]; then
  EXTENSION_CHANGED=true
  EXT_STAGE="$EXT_ROOT/.${EXT_UUID}.stage-$$"
  EXT_PREVIOUS="$EXT_ROOT/.${EXT_UUID}.previous-$$"
  rm -rf "$EXT_STAGE" "$EXT_PREVIOUS"
  cp -a "$EXT_SOURCE" "$EXT_STAGE"
  if [[ -d "$EXT_TARGET" ]]; then mv "$EXT_TARGET" "$EXT_PREVIOUS"; fi
  mv "$EXT_STAGE" "$EXT_TARGET"
  rm -rf "$EXT_PREVIOUS"
fi

if $EXTENSION_CHANGED; then
  cat <<MSG
The GNOME integration files are installed. At your convenience, log out and
back in once so Shell loads the on-disk version, then run this if the indicator
is not enabled:

  gnome-extensions enable '$EXT_UUID'
MSG
fi

AUTOSTART_FILE="$AUTOSTART_DIR/agents-usage.desktop"
if $AUTOSTART || [[ -f "$AUTOSTART_FILE" ]]; then
  mkdir -p "$AUTOSTART_DIR"
  sed "s|@EXEC@|$BIN_DIR/agents-usage|g" \
    "$BUNDLE/share/applications/agents-usage-autostart.desktop.in" \
    > "$AUTOSTART_FILE"
  chmod 0644 "$AUTOSTART_FILE"
  if $AUTOSTART; then
    echo "Autostart enabled."
  else
    echo "Existing autostart entry updated."
  fi
fi

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true

echo
echo "Agents Usage installed for $USER."
echo "Binary: $BIN_DIR/agents-usage"
echo "Launcher: $APP_DIR/agents-usage.desktop"
echo "You can now start it from the GNOME app grid as 'Agents Usage' or run:"
echo "  $BIN_DIR/agents-usage --open"
echo "If Agents Usage was already running, use its Quit menu and launch it again"
echo "when convenient to begin using the newly installed binary."
