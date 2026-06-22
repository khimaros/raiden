#!/bin/sh
# raiden bootstrap: drop a prebuilt static raiden binary into a live environment,
# so installing on real hardware needs no rust toolchain. run as root (or set
# RAIDEN_DEST to a writable path):
#
#   wget -qO- https://raw.githubusercontent.com/khimaros/raiden/main/install.sh | sh
#
# overrides: RAIDEN_VERSION=v0.2.0 (default: latest release), RAIDEN_DEST=/path.
# the binary is the `make dist` artifact published on the github release page.
set -eu

REPO="khimaros/raiden"
ASSET="raiden-x86_64-linux-musl"
VERSION="${RAIDEN_VERSION:-latest}"
DEST="${RAIDEN_DEST:-/usr/local/bin/raiden}"

arch="$(uname -m)"
if [ "$arch" != "x86_64" ]; then
    echo "no prebuilt binary for $arch (only x86_64); build from source with 'make dist'." >&2
    exit 1
fi

if [ "$VERSION" = "latest" ]; then
    url="https://github.com/$REPO/releases/latest/download/$ASSET"
else
    url="https://github.com/$REPO/releases/download/$VERSION/$ASSET"
fi

echo "downloading $url"
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$DEST"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$DEST" "$url"
else
    echo "need curl or wget to download raiden." >&2
    exit 1
fi
chmod +x "$DEST"

echo "installed raiden to $DEST"
echo "next: raiden install   # discovers disks, generates a config, then provisions"
