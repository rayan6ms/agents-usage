#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for tool in cargo podman rpmbuild flatpak curl desktop-file-validate appstreamcli; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "Required packaging tool is missing: $tool" >&2
    exit 1
  fi
done

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
HOST_ARCH="$(uname -m)"
case "$HOST_ARCH" in
  x86_64) RPM_ARCH=x86_64; DEB_ARCH=amd64; APPIMAGE_ARCH=x86_64 ;;
  aarch64) RPM_ARCH=aarch64; DEB_ARCH=arm64; APPIMAGE_ARCH=aarch64 ;;
  *) echo "Unsupported package architecture: $HOST_ARCH" >&2; exit 1 ;;
esac

./tools/build-release.sh

DIST="$ROOT/dist"
PACKAGES="$DIST/packages"
WORK="$DIST/.package-work"
CACHE="$ROOT/.packaging-cache"
PORTABLE_TARGET="$ROOT/target/portable"
PORTABLE_BINARY="$PORTABLE_TARGET/release/agents-usage"
APP_ID="io.github.agentsusagetray.AgentsUsage"
EXT_UUID="agents-usage@local"

rm -rf "$PACKAGES" "$WORK"
mkdir -p "$PACKAGES" "$WORK" "$CACHE/cargo-registry" "$CACHE/cargo-git"

echo "Building a glibc 2.36-compatible release binary with rootless Podman..."
podman build \
  --file "$ROOT/packaging/linux/Containerfile.portable" \
  --tag localhost/agents-usage-builder:bookworm \
  "$ROOT/packaging/linux"
podman run --rm \
  --volume "$ROOT:/workspace:Z" \
  --volume "$CACHE/cargo-registry:/usr/local/cargo/registry:Z" \
  --volume "$CACHE/cargo-git:/usr/local/cargo/git:Z" \
  --env CARGO_TARGET_DIR=/workspace/target/portable \
  localhost/agents-usage-builder:bookworm \
  cargo build --release --locked
test -x "$PORTABLE_BINARY"

MAX_GLIBC="$(readelf --version-info "$PORTABLE_BINARY" | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -n 1)"
if [[ "${MAX_GLIBC#GLIBC_}" != "$(printf '%s\n' "${MAX_GLIBC#GLIBC_}" 2.36 | sort -V | head -n 1)" ]]; then
  echo "Portable binary unexpectedly requires $MAX_GLIBC (maximum supported is GLIBC_2.36)." >&2
  exit 1
fi
echo "Portable binary baseline: $MAX_GLIBC"

NATIVE="$WORK/native-root"
install -d \
  "$NATIVE/usr/bin" \
  "$NATIVE/usr/share/applications" \
  "$NATIVE/usr/share/icons/hicolor/scalable/apps" \
  "$NATIVE/usr/share/metainfo" \
  "$NATIVE/usr/share/gnome-shell/extensions" \
  "$NATIVE/usr/share/doc/agents-usage"
install -m 0755 "$PORTABLE_BINARY" "$NATIVE/usr/bin/agents-usage"
sed 's|"@EXEC@"|agents-usage|' packaging/linux/agents-usage.desktop.in \
  > "$NATIVE/usr/share/applications/$APP_ID.desktop"
install -m 0644 packaging/linux/agents-usage.svg \
  "$NATIVE/usr/share/icons/hicolor/scalable/apps/agents-usage.svg"
install -m 0644 packaging/linux/io.github.agentsusagetray.AgentsUsage.metainfo.xml \
  "$NATIVE/usr/share/metainfo/io.github.agentsusagetray.AgentsUsage.metainfo.xml"
cp -a integration/gnome-shell/extension \
  "$NATIVE/usr/share/gnome-shell/extensions/$EXT_UUID"
install -m 0644 README.md LICENSE THIRD_PARTY_ASSETS.md \
  "$NATIVE/usr/share/doc/agents-usage/"
cp -a third_party "$NATIVE/usr/share/doc/agents-usage/third_party"

desktop-file-validate "$NATIVE/usr/share/applications/$APP_ID.desktop"
appstreamcli validate --no-net \
  --override=url-homepage-missing=info \
  "$NATIVE/usr/share/metainfo/$APP_ID.metainfo.xml"

echo "Building RPM package..."
RPM_TOP="$WORK/rpmbuild"
mkdir -p "$RPM_TOP"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
sed \
  -e "s|@VERSION@|$VERSION|g" \
  -e "s|@RPM_ARCH@|$RPM_ARCH|g" \
  packaging/linux/agents-usage.spec.in > "$RPM_TOP/SPECS/agents-usage.spec"
rpmbuild -bb \
  --define "_topdir $RPM_TOP" \
  --define "_stage $NATIVE" \
  "$RPM_TOP/SPECS/agents-usage.spec"
RPM_FILE="$(find "$RPM_TOP/RPMS" -type f -name '*.rpm' -print -quit)"
install -m 0644 "$RPM_FILE" "$PACKAGES/agents-usage-$VERSION-1.$RPM_ARCH.rpm"

echo "Building DEB package..."
DEB_ROOT="$WORK/deb-root"
mkdir -p "$DEB_ROOT/DEBIAN"
cp -a "$NATIVE/usr" "$DEB_ROOT/usr"
sed \
  -e "s|@VERSION@|$VERSION|g" \
  -e "s|@DEB_ARCH@|$DEB_ARCH|g" \
  packaging/linux/debian-control.in > "$DEB_ROOT/DEBIAN/control"
if command -v dpkg-deb >/dev/null 2>&1; then
  dpkg-deb --root-owner-group --build "$DEB_ROOT" \
    "$PACKAGES/agents-usage_${VERSION}_${DEB_ARCH}.deb"
else
  podman run --rm \
    --volume "$DEB_ROOT:/package:Z" \
    --volume "$PACKAGES:/output:Z" \
    docker.io/library/debian:bookworm-slim \
    dpkg-deb --root-owner-group --build /package "/output/agents-usage_${VERSION}_${DEB_ARCH}.deb"
fi

echo "Building Flatpak bundle..."
FLAT_BUILD="$WORK/flatpak-build"
FLAT_REPO="$WORK/flatpak-repo"
flatpak build-init "$FLAT_BUILD" "$APP_ID" \
  org.freedesktop.Sdk/$HOST_ARCH/25.08 org.freedesktop.Platform/$HOST_ARCH/25.08 25.08
install -d \
  "$FLAT_BUILD/files/bin" \
  "$FLAT_BUILD/files/libexec" \
  "$FLAT_BUILD/files/share/applications" \
  "$FLAT_BUILD/files/share/icons/hicolor/scalable/apps" \
  "$FLAT_BUILD/files/share/metainfo" \
  "$FLAT_BUILD/files/share/gnome-shell/extensions"
install -m 0755 "$PORTABLE_BINARY" "$FLAT_BUILD/files/libexec/agents-usage"
install -m 0755 packaging/linux/portable-launcher.sh "$FLAT_BUILD/files/bin/agents-usage"
sed \
  -e 's|"@EXEC@"|agents-usage|' \
  -e "s|Icon=agents-usage|Icon=$APP_ID|" \
  packaging/linux/agents-usage.desktop.in \
  > "$FLAT_BUILD/files/share/applications/$APP_ID.desktop"
install -m 0644 packaging/linux/agents-usage.svg \
  "$FLAT_BUILD/files/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -m 0644 packaging/linux/io.github.agentsusagetray.AgentsUsage.metainfo.xml \
  "$FLAT_BUILD/files/share/metainfo/$APP_ID.metainfo.xml"
cp -a integration/gnome-shell/extension \
  "$FLAT_BUILD/files/share/gnome-shell/extensions/$EXT_UUID"
flatpak build "$FLAT_BUILD" sh -c 'ldd /app/libexec/agents-usage >/dev/null'
flatpak build-finish \
  --command=agents-usage \
  --share=ipc \
  --share=network \
  --socket=x11 \
  --socket=session-bus \
  --filesystem=home \
  --own-name=io.github.agentsusagetray.App \
  --talk-name=org.freedesktop.Flatpak \
  "$FLAT_BUILD"
flatpak build-export "$FLAT_REPO" "$FLAT_BUILD" stable
flatpak build-bundle \
  --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo \
  "$FLAT_REPO" "$PACKAGES/agents-usage-$VERSION-$HOST_ARCH.flatpak" "$APP_ID" stable

echo "Building AppImage..."
APPDIR="$WORK/Agents_Usage.AppDir"
install -d \
  "$APPDIR/usr/lib" \
  "$APPDIR/usr/libexec" \
  "$APPDIR/usr/share/applications" \
  "$APPDIR/usr/share/icons/hicolor/scalable/apps" \
  "$APPDIR/usr/share/metainfo" \
  "$APPDIR/usr/share/gnome-shell/extensions"
install -m 0755 "$PORTABLE_BINARY" "$APPDIR/usr/libexec/agents-usage"
install -m 0755 packaging/linux/portable-launcher.sh "$APPDIR/AppRun"
sed 's|"@EXEC@"|agents-usage|' packaging/linux/agents-usage.desktop.in \
  > "$APPDIR/$APP_ID.desktop"
install -m 0644 "$APPDIR/$APP_ID.desktop" \
  "$APPDIR/usr/share/applications/$APP_ID.desktop"
install -m 0644 packaging/linux/agents-usage.svg "$APPDIR/agents-usage.svg"
install -m 0644 packaging/linux/agents-usage.svg \
  "$APPDIR/usr/share/icons/hicolor/scalable/apps/agents-usage.svg"
install -m 0644 packaging/linux/io.github.agentsusagetray.AgentsUsage.metainfo.xml \
  "$APPDIR/usr/share/metainfo/io.github.agentsusagetray.AgentsUsage.metainfo.xml"
cp -a integration/gnome-shell/extension \
  "$APPDIR/usr/share/gnome-shell/extensions/$EXT_UUID"

podman run --rm \
  --volume "$ROOT:/workspace:Z" \
  --volume "$APPDIR:/appdir:Z" \
  localhost/agents-usage-builder:bookworm \
  sh -c 'ldd /workspace/target/portable/release/agents-usage | while read -r soname arrow library rest; do [ "$arrow" = "=>" ] || continue; case "$library" in /lib/*|/usr/lib/*) ;; *) continue ;; esac; case "$(basename "$library")" in libc.so.*|libm.so.*|libpthread.so.*|librt.so.*|libdl.so.*|ld-linux-*) continue ;; esac; cp -L "$library" /appdir/usr/lib/; done'

APPIMAGETOOL="$CACHE/appimagetool-$APPIMAGE_ARCH.AppImage"
if [[ ! -x "$APPIMAGETOOL" ]]; then
  curl --fail --location \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$APPIMAGE_ARCH.AppImage" \
    --output "$APPIMAGETOOL"
  chmod 0755 "$APPIMAGETOOL"
fi
ARCH="$APPIMAGE_ARCH" "$APPIMAGETOOL" --appimage-extract-and-run \
  "$APPDIR" "$PACKAGES/Agents_Usage-$VERSION-$APPIMAGE_ARCH.AppImage"
chmod 0755 "$PACKAGES/Agents_Usage-$VERSION-$APPIMAGE_ARCH.AppImage"

./tools/verify-packages.sh "$PACKAGES"

printf '\nLinux packages are ready in %s\n' "$PACKAGES"
find "$PACKAGES" -maxdepth 1 -type f -printf '%f\n' | sort
