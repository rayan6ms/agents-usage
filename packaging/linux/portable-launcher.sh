#!/bin/sh
set -eu

if [ -n "${APPDIR:-}" ]; then
    app_prefix="$APPDIR/usr"
else
    app_prefix="/app"
fi

# Portable formats cannot register a system-wide GNOME extension at install
# time. Keep the bundled companion in the per-user extension directory so the
# application still has its tray-side integration after GNOME reloads it.
extension_uuid="agents-usage@local"
extension_source="$app_prefix/share/gnome-shell/extensions/$extension_uuid"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
extension_target="$data_home/gnome-shell/extensions/$extension_uuid"
if [ -d "$extension_source" ]; then
    source_version="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$extension_source/metadata.json" | head -n 1)"
    target_version=""
    if [ -f "$extension_target/metadata.json" ]; then
        target_version="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$extension_target/metadata.json" | head -n 1)"
    fi
    if [ "$source_version" != "$target_version" ]; then
        mkdir -p "$(dirname "$extension_target")"
        extension_stage="$(dirname "$extension_target")/.${extension_uuid}.stage-$$"
        extension_previous="$(dirname "$extension_target")/.${extension_uuid}.previous-$$"
        rm -rf "$extension_stage" "$extension_previous"
        cp -a "$extension_source" "$extension_stage"
        if [ -d "$extension_target" ]; then mv "$extension_target" "$extension_previous"; fi
        mv "$extension_stage" "$extension_target"
        rm -rf "$extension_previous"
    fi
fi

exec "$app_prefix/libexec/agents-usage" "$@"
