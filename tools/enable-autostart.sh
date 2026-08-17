#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$HOME/.local/bin/agents-usage"
if [[ ! -x "$BIN" ]]; then
  echo "Agents Usage is not installed yet. Run ./tools/install-user.sh first."
  exit 1
fi
mkdir -p "$HOME/.config/autostart"
sed "s|@EXEC@|$BIN|g" "$ROOT/packaging/linux/agents-usage-autostart.desktop.in" > "$HOME/.config/autostart/agents-usage.desktop"
chmod 0644 "$HOME/.config/autostart/agents-usage.desktop"
echo "Agents Usage autostart enabled."
