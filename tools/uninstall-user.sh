#!/usr/bin/env bash
set -euo pipefail
EXT_UUID="agents-usage@local"
gnome-extensions disable "$EXT_UUID" >/dev/null 2>&1 || true
rm -rf "$HOME/.local/share/gnome-shell/extensions/$EXT_UUID"
rm -f "$HOME/.local/bin/agents-usage"
rm -f "$HOME/.local/share/applications/agents-usage.desktop"
rm -f "$HOME/.local/share/icons/hicolor/scalable/apps/agents-usage.svg"
rm -f "$HOME/.config/autostart/agents-usage.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$HOME/.local/share/applications" >/dev/null 2>&1 || true
echo "Agents Usage application files removed."
echo "Your settings in ~/.config/agents-usage were left intact."
