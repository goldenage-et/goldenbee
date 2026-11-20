#!/bin/sh
# Install Goldenbee directly from a GitHub release.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- client
#   curl -fsSL .../install.sh | sh -s -- all
#
# Optional environment:
#   GOLDENBEE_VERSION=v0.1.0
#   GOLDENBEE_BIN_DIR="$HOME/.local/bin"
#   GOLDENBEE_CONFIG_DIR="$HOME/.config/goldenbee"
#   GOLDENBEE_DATA_DIR="$HOME/.local/share/goldenbee"
#   GOLDENBEE_REPO=goldenage-et/goldenbee
#   GOLDENBEE_RELEASE_BASE=https://github.com/goldenage-et/goldenbee/releases
#   GOLDENBEE_NO_CONFIG=1
set -eu

usage() {
  echo "Usage: install.sh [gateway|client|all]" >&2
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

component="${1:-gateway}"
case "$component" in
  gateway|client|all) ;;
  *)
    usage
    exit 2
    ;;
esac

repo="${GOLDENBEE_REPO:-goldenage-et/goldenbee}"
version="${GOLDENBEE_VERSION:-latest}"
bin_dir="${GOLDENBEE_BIN_DIR:-$HOME/.local/bin}"
config_dir="${GOLDENBEE_CONFIG_DIR:-$HOME/.config/goldenbee}"
data_dir="${GOLDENBEE_DATA_DIR:-$HOME/.local/share/goldenbee}"
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

temporary="$(mktemp -d "${TMPDIR:-/tmp}/gb-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

curl -fL --retry 3 --proto '=https' --tlsv1.2 -sS \
  -o "$temporary/SHA256SUMS" "$download_root/SHA256SUMS"

install_asset() {
  name="$1"
  asset="$name-x86_64-unknown-linux-gnu"
  destination="$bin_dir/$name"
  curl -fL --retry 3 --proto '=https' --tlsv1.2 -sS \
    -o "$temporary/$asset" "$download_root/$asset"
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
  mv -f "$temporary/$asset" "$destination"
  echo "gb: installed $destination"
}

write_gateway_config() {
  if [ "${GOLDENBEE_NO_CONFIG:-0}" = "1" ]; then
    return
  fi
  mkdir -p "$config_dir" "$config_dir/workers" \
    "$data_dir/gateway" "$data_dir/instances" "$data_dir/packages" \
    "$data_dir/workspaces" "$data_dir/releases"
  if [ ! -f "$config_dir/gateway.json" ]; then
    token="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')"
    cat >"$config_dir/gateway.json" <<INSTALL_EOF
{
  "listen": "127.0.0.1:8080",
  "data_dir": "$data_dir/gateway",
  "registry": "$data_dir/gateway-registry.json",
  "lease_ttl_ms": 30000,
  "callers": [{
    "token": "$token",
    "id": "local",
    "namespace": "local",
    "scopes": ["*"]
  }],
  "host_id": "local",
  "instances_root": "$data_dir/instances",
  "packages_root": "$data_dir/packages",
  "trust_policy": "$config_dir/trust.json",
  "allowed_package_sources": ["$data_dir/releases"],
  "operations": ["provision", "inspect", "upgrade", "rollback", "start", "stop", "restart"],
  "allowed_grants": ["task:submit", "task:read", "task:cancel", "session:write", "profile:read", "profile:write"],
  "allowed_profile_grants": ["task:submit", "task:read"],
  "allowed_config_roots": ["$config_dir/workers"],
  "allowed_workspace_roots": ["$data_dir/workspaces"],
  "allowed_secret_prefixes": ["secret://"],
  "allow_remote_listener": false
}
INSTALL_EOF
    chmod 0600 "$config_dir/gateway.json"
    echo "gb: wrote $config_dir/gateway.json"
  fi
  if [ ! -f "$config_dir/trust.json" ]; then
    cat >"$config_dir/trust.json" <<'INSTALL_EOF'
{
  "trusted_signers": {},
  "publisher_tiers": {},
  "allowed_tiers": ["official", "verified", "community"],
  "targets": ["x86_64-unknown-linux-gnu"],
  "runtimes": [],
  "adks": [],
  "protocols": ["1.0"],
  "features": [],
  "approved_permissions": [],
  "sensitive_permissions": []
}
INSTALL_EOF
    chmod 0600 "$config_dir/trust.json"
    echo "gb: wrote $config_dir/trust.json (add trusted signers, runtimes, ADKs, and permissions)"
  fi
}

case "$component" in
  gateway)
    install_asset gb-gateway
    write_gateway_config
    ;;
  client)
    install_asset gb-client
    ;;
  all)
    install_asset gb-gateway
    install_asset gb-client
    write_gateway_config
    ;;
esac

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "note: add $bin_dir to PATH" ;;
esac

if [ "$component" != "client" ]; then
  echo "next:"
  echo "  edit $config_dir/trust.json"
  echo "  $bin_dir/gb-gateway serve --config $config_dir/gateway.json"
fi
