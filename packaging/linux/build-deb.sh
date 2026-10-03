#!/usr/bin/env bash
# Builds a .deb from a `flutter build linux --release` bundle.
#
#   packaging/linux/build-deb.sh <version> [bundle dir] [output dir]
set -euo pipefail
version="$1"
root="$(cd "$(dirname "$0")/../.." && pwd)"
bundle="${2:-$root/app/build/linux/x64/release/bundle}"
out="${3:-$root/dist}"
here="$root/packaging/linux"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
pkg="$stage/kilonova_${version}_amd64"

mkdir -p "$pkg/DEBIAN" "$pkg/opt/kilonova" "$pkg/usr/bin" \
  "$pkg/usr/share/applications" "$pkg/usr/share/metainfo"
cp -r "$bundle"/. "$pkg/opt/kilonova/"
ln -s /opt/kilonova/kilonova "$pkg/usr/bin/kilonova"
cp "$here/com.dhivalabs.kilonova.desktop" "$pkg/usr/share/applications/"
cp "$here/com.dhivalabs.kilonova.metainfo.xml" "$pkg/usr/share/metainfo/"
for size in 64 128 256 512; do
  dir="$pkg/usr/share/icons/hicolor/${size}x${size}/apps"
  mkdir -p "$dir"
  cp "$here/icons/${size}x${size}/com.dhivalabs.kilonova.png" "$dir/"
done

# Reproducible build: staged files otherwise carry today's mtime, which
# ends up in control.tar.gz/data.tar.gz inside the .deb. If the caller
# gives us a SOURCE_DATE_EPOCH (the release workflow sets it to the tagged
# commit's time), pin every staged file to it.
if [[ -n "${SOURCE_DATE_EPOCH:-}" ]]; then
  find "$pkg" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
fi

installed_kb="$(du -sk "$pkg" | cut -f1)"
cat > "$pkg/DEBIAN/control" <<CONTROL
Package: kilonova
Version: ${version}
Architecture: amd64
Maintainer: Dhiva Labs <reachout@dhivalabs.com>
Installed-Size: ${installed_kb}
Depends: libgtk-3-0t64 | libgtk-3-0, libstdc++6
Section: utils
Priority: optional
Homepage: https://github.com/Dhiva-Labs/kilonova
Description: Monero wallet for full nodes and light wallet servers
 Kilonova scans the Monero blockchain on your device from a node you choose,
 or syncs through a light wallet server and checks what it reports with your
 own keys. Not audited yet; keep only small amounts in it.
CONTROL

mkdir -p "$out"
dpkg-deb --root-owner-group --build "$pkg" "$out/kilonova_${version}_amd64.deb"
echo "$out/kilonova_${version}_amd64.deb"
