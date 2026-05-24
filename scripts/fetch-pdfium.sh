#!/usr/bin/env bash
set -euo pipefail

PDFIUM_VERSION="chromium/7843"

plat="${1:-}"
if [ -z "$plat" ]; then
  echo "usage: $0 <linux-x64|linux-arm64|mac-x64|mac-arm64|win-x64|win-arm64>" >&2
  exit 2
fi

case "$plat" in
  linux-x64)   archive="pdfium-linux-x64";   localdir="linux-x64";   sha256="22a5c578250fb4f54e69b395ebb96e4da108b8a0ec4c7e6cf94e649867f5c63d" ;;
  linux-arm64) archive="pdfium-linux-arm64"; localdir="linux-arm64"; sha256="c26eae94885216ba7ae25cdbf1263df37dc13181ebfb148becbdca3f1d9a5040" ;;
  mac-x64)     archive="pdfium-mac-x64";     localdir="macos-x64";   sha256="9aabdf80c7eb37a1e5af609a8a63268f11ba5c9d4129a3b16b2deac3dcb8b3f9" ;;
  mac-arm64)   archive="pdfium-mac-arm64";   localdir="macos-arm64"; sha256="34015b412ded9faeb4d7118b25307b1a43bbdad7a3d296161357b9fd090f47aa" ;;
  win-x64)     archive="pdfium-win-x64";     localdir="win-x64";     sha256="242d19fa80aae8483aa60e75cac9d11fa50cce8a7771ba7a69cd7e00b99a8f24" ;;
  win-arm64)   archive="pdfium-win-arm64";   localdir="win-arm64";   sha256="ebe87dd637225a70fea6a6bd94c50fef03c8a093212f3702e929d8b26a387e80" ;;
  *) echo "unknown platform: $plat" >&2; exit 2 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/pdfium/$localdir"
url="https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_VERSION}/${archive}.tgz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $url"
curl -fSL "$url" -o "$tmp/pdfium.tgz"

echo "Verifying SHA-256 ($sha256)"
if command -v sha256sum >/dev/null 2>&1; then
  echo "${sha256}  ${tmp}/pdfium.tgz" | sha256sum -c -
elif command -v shasum >/dev/null 2>&1; then
  echo "${sha256}  ${tmp}/pdfium.tgz" | shasum -a 256 -c -
else
  echo "no SHA-256 tool (sha256sum or shasum) found" >&2
  exit 1
fi

mkdir -p "$tmp/x"
tar -xzf "$tmp/pdfium.tgz" -C "$tmp/x"

mkdir -p "$dest"
cp -r "$tmp/x/." "$dest/"
echo "Installed PDFium ${PDFIUM_VERSION} for $plat into $dest"
ls "$dest/lib" 2>/dev/null || true
