#!/usr/bin/env bash
set -euo pipefail

source_key="${ANDROID_KEYSTORE_PATH:-${XDG_CONFIG_HOME:-$HOME/.config}/agents-usage/android-release/agents-usage-release.jks}"
destination="${1:-}"

if [[ -z "$destination" ]]; then
    echo "Usage: $0 /path/on/offline-media/agents-usage-android-key.jks.enc" >&2
    exit 2
fi
if [[ ! -f "$source_key" ]]; then
    echo "Signing key not found: $source_key" >&2
    exit 1
fi

repository_root="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"
destination_parent="$(realpath -m "$(dirname "$destination")")"
destination_path="$destination_parent/$(basename "$destination")"
if [[ -e "$destination_path" ]]; then
    echo "Refusing to overwrite an existing backup: $destination_path" >&2
    exit 1
fi
case "$destination_path" in
    "$repository_root"/*)
        echo "Refusing to place a signing-key backup inside the repository." >&2
        exit 1
        ;;
esac

mkdir -p "$destination_parent"
temporary="$(mktemp "$destination_parent/.agents-usage-key.XXXXXX")"
trap 'rm -f "$temporary"' EXIT
chmod 600 "$temporary"
openssl enc -aes-256-cbc -pbkdf2 -salt -in "$source_key" -out "$temporary"
mv "$temporary" "$destination_path"
trap - EXIT
chmod 600 "$destination_path"

echo "Encrypted signing-key backup created at: $destination_path"
echo "Keep its encryption password in a separate password manager and test restoration before release."
