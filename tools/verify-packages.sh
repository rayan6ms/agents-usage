#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGE_DIR="${1:-$ROOT/dist/packages}"
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
HOST_ARCH="$(uname -m)"
case "$HOST_ARCH" in
  x86_64) RPM_ARCH=x86_64; DEB_ARCH=amd64; APPIMAGE_ARCH=x86_64 ;;
  aarch64) RPM_ARCH=aarch64; DEB_ARCH=arm64; APPIMAGE_ARCH=aarch64 ;;
  *) echo "Unsupported verification architecture: $HOST_ARCH" >&2; exit 1 ;;
esac

RPM="$PACKAGE_DIR/agents-usage-$VERSION-1.$RPM_ARCH.rpm"
DEB="$PACKAGE_DIR/agents-usage_${VERSION}_${DEB_ARCH}.deb"
FLATPAK="$PACKAGE_DIR/agents-usage-$VERSION-$HOST_ARCH.flatpak"
APPIMAGE="$PACKAGE_DIR/Agents_Usage-$VERSION-$APPIMAGE_ARCH.AppImage"
for artifact in "$RPM" "$DEB" "$FLATPAK" "$APPIMAGE"; do
  test -s "$artifact" || { echo "Missing package: $artifact" >&2; exit 1; }
done

rpm -K "$RPM"
test "$(rpm -qp --qf '%{VERSION}-%{RELEASE}.%{ARCH}' "$RPM")" = "$VERSION-1.$RPM_ARCH"
rpm -qpl "$RPM" | grep -q '/usr/bin/agents-usage$'
rpm -qpl "$RPM" | grep -q '/usr/share/gnome-shell/extensions/agents-usage@local/metadata.json$'

if command -v dpkg-deb >/dev/null 2>&1; then
  dpkg-deb --info "$DEB" >/dev/null
  test "$(dpkg-deb --field "$DEB" Version)" = "$VERSION"
  test "$(dpkg-deb --field "$DEB" Architecture)" = "$DEB_ARCH"
  dpkg-deb --contents "$DEB" | grep -q 'usr/bin/agents-usage$'
  dpkg-deb --contents "$DEB" | grep -q 'usr/share/gnome-shell/extensions/agents-usage@local/metadata.json$'
else
  podman run --rm --volume "$DEB:/package.deb:ro,Z" \
    docker.io/library/debian:bookworm-slim sh -c \
    'dpkg-deb --info /package.deb >/dev/null && dpkg-deb --contents /package.deb > /tmp/package-contents && grep -q "usr/bin/agents-usage$" /tmp/package-contents'
fi

VERIFY_WORK="$(mktemp -d "$PACKAGE_DIR/.verify.XXXXXX")"
trap 'rm -rf "$VERIFY_WORK"' EXIT
flatpak build-init "$VERIFY_WORK/seed-build" io.github.agentsusagetray.Verify \
  org.freedesktop.Sdk/$HOST_ARCH/25.08 org.freedesktop.Platform/$HOST_ARCH/25.08 25.08
flatpak build-finish --command=true "$VERIFY_WORK/seed-build"
flatpak build-export "$VERIFY_WORK/repo" "$VERIFY_WORK/seed-build" verify >/dev/null
flatpak build-import-bundle "$VERIFY_WORK/repo" "$FLATPAK" >/dev/null
flatpak repo \
  --metadata="app/io.github.agentsusagetray.AgentsUsage/$HOST_ARCH/stable" \
  "$VERIFY_WORK/repo" | grep -q 'shared=.*network'

(
  cd "$VERIFY_WORK"
  "$APPIMAGE" --appimage-extract >/dev/null
  test -x squashfs-root/AppRun
  test -x squashfs-root/usr/libexec/agents-usage
  test -f squashfs-root/usr/share/metainfo/io.github.agentsusagetray.AgentsUsage.metainfo.xml
  test -f squashfs-root/usr/share/gnome-shell/extensions/agents-usage@local/metadata.json
  linked="$(LD_LIBRARY_PATH="$PWD/squashfs-root/usr/lib" ldd squashfs-root/usr/libexec/agents-usage)"
  ! grep -q 'not found' <<<"$linked"
)

echo "RPM, DEB, Flatpak, and AppImage verification: PASS"
