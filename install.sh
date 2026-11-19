#!/bin/sh
# Install the Goldenbee operator bootstrap.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh
#
# Optional environment:
#   GOLDENBEE_VERSION=v0.1.0
#   GOLDENBEE_BIN_DIR="$HOME/.local/bin"
#   GOLDENBEE_REPO=goldenage-et/goldenbee
#   GOLDENBEE_RELEASE_BASE=https://github.com/goldenage-et/goldenbee/releases
set -eu

repo="${GOLDENBEE_REPO:-goldenage-et/goldenbee}"
version="${GOLDENBEE_VERSION:-latest}"
bin_dir="${GOLDENBEE_BIN_DIR:-$HOME/.local/bin}"
release_base="${GOLDENBEE_RELEASE_BASE:-https://github.com/$repo/releases}"

case "$(uname -s 2>/dev/null || true)" in
  Linux) ;;
  *) echo "gb: only Linux is supported by this installer" >&2; exit 1 ;;
esac
case "$(uname -m 2>/dev/null || true)" in
  x86_64|amd64) ;;
  *) echo "gb: only x86_64 Linux is supported by this installer" >&2; exit 1 ;;
esac
command -v curl >/dev/null 2>&1 || {
  echo "gb: curl is required" >&2
  exit 1
}
command -v sha256sum >/dev/null 2>&1 || {
  echo "gb: sha256sum is required" >&2
  exit 1
}

if [ "$version" = "latest" ]; then
  download_root="$release_base/latest/download"
else
  case "$version" in
    v*) tag="$version" ;;
    *) tag="v$version" ;;
  esac
  download_root="$release_base/download/$tag"
fi

asset="gb-x86_64-unknown-linux-gnu"
temporary="$(mktemp -d "${TMPDIR:-/tmp}/gb-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

curl -fL --retry 3 --proto '=https' --tlsv1.2 -sS \
  -o "$temporary/$asset" "$download_root/$asset"
curl -fL --retry 3 --proto '=https' --tlsv1.2 -sS \
  -o "$temporary/SHA256SUMS" "$download_root/SHA256SUMS"

expected="$(awk -v name="$asset" '$2 == name || $2 == "*" name { print $1; exit }' "$temporary/SHA256SUMS")"
if [ -z "$expected" ]; then
  echo "gb: SHA256SUMS does not contain $asset" >&2
  exit 1
fi
actual="$(sha256sum "$temporary/$asset" | awk '{ print $1 }')"
if [ "$actual" != "$expected" ]; then
  echo "gb: checksum mismatch for $asset" >&2
  exit 1
fi

mkdir -p "$bin_dir"
chmod 0755 "$temporary/$asset"
mv -f "$temporary/$asset" "$bin_dir/gb"
echo "gb: installed $bin_dir/gb"
echo "next:"
echo "  gb i gateway"
echo "  gb i composer"
echo "  gb i client-cli"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "note: add $bin_dir to PATH" ;;
esac
