#!/usr/bin/env bash
# Assemble already-tested binaries. No compilation, target override, or upload.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$ROOT/tools/release.py" "$@"
