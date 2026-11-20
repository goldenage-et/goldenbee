#!/usr/bin/env bash
# Ticket 47 cross-process security acceptance runner.
#
# Compiles test binaries first (cargo test --no-run; never cargo build), then
# runs each owned suite, preserving logs and printing one deterministic failure
# summary. Set GBB_SHARED=1 to route the commands through gbb.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
export TMPDIR="${TMPDIR:-/var/tmp/goldenbee-47}"
mkdir -p "$TMPDIR"
LOG_DIR="$ROOT/target/security-47"
mkdir -p "$LOG_DIR"

if [[ "${GBB_SHARED:-0}" == "1" ]]; then
    launcher=(gbb --shared --wait 900)
else
    launcher=(cargo)
fi

failures=()
passed=0
total=0

run() {
    local label="$1"
    shift
    total=$((total + 1))
    local slug
    slug="$(printf '%s' "$label" | tr ' /' '__')"
    local log="$LOG_DIR/$slug.log"
    echo "[security-47] run: $label"
    printf '[security-47] command: %q ' "${launcher[@]}" >"$log"
    printf '%q ' "$@" >>"$log"
    printf '\n' >>"$log"
    if "${launcher[@]}" "$@" >>"$log" 2>&1; then
        passed=$((passed + 1))
        echo "[security-47] pass: $label"
    else
        failures+=("$label ($log)")
        echo "[security-47] FAIL: $label"
        tail -n 30 "$log" | sed 's/^/  | /'
    fi
}

run_external() {
    local label="$1"
    shift
    total=$((total + 1))
    local slug
    slug="$(printf '%s' "$label" | tr ' /' '__')"
    local log="$LOG_DIR/$slug.log"
    echo "[security-47] run: $label"
    printf '[security-47] command: ' >"$log"
    printf '%q ' "$@" >>"$log"
    printf '\n' >>"$log"
    if "$@" >>"$log" 2>&1; then
        passed=$((passed + 1))
        echo "[security-47] pass: $label"
    else
        failures+=("$label ($log)")
        echo "[security-47] FAIL: $label"
        tail -n 30 "$log" | sed 's/^/  | /'
    fi
}

# Build every binary the cross-process fixtures start, without invoking build.
run compile \
    test --no-run \
    -p goldenbee-gateway -p general -p runtime -p install -p instance \
    -p knowledge -p echo -p coding -p graph -p goldenbee-client-cli --all-features
run_external gb-gateway \
    cargo run -q -p goldenbee-gateway -- --help
run_external gb-client \
    cargo run -q -p goldenbee-client-cli -- --help

run gateway-security \
    test -p goldenbee-gateway --test security -- --test-threads=1 --nocapture
run worker-security \
    test -p general --test security -- --test-threads=1 --nocapture

# Forced termination, approvals, receipts, and child dispatch.
run approval \
    test -p goldenbee-gateway --test approval -- --test-threads=1
run recovery \
    test -p goldenbee-gateway --test recovery -- --test-threads=1
run effects \
    test -p general --test effect -- --test-threads=1

# Malicious retrieved/peer/MCP/repository input and OS isolation.
run peer \
    test -p general --test peer -- --test-threads=1
run mcp-remote \
    test -p general --test mcp_remote -- --test-threads=1
run sandbox \
    test -p runtime --test sandbox -- --test-threads=1 \
    --skip separate_boundaries_use_independent_identities_and_resource_policies
run knowledge \
    test -p runtime --test knowledge -- --test-threads=1

# Package trust, provisioning, registry outage, and offline execution.
run install-trust \
    test -p install --test trust -- --test-threads=1
run instance-trust \
    test -p instance --test trust -- --test-threads=1
run fleet \
    test -p general --test fleet -- --test-threads=1

# Hook ordering, independent config/assets, and Launcher without Gateway.
run hooks \
    test -p general --test hooks -- --test-threads=1
run config \
    test -p general --test config -- --test-threads=1
run profiles \
    test -p general --test profile -- --test-threads=1
run launcher \
    test -p knowledge --test launcher -- --test-threads=1
run_external dependencies \
    python3 tests/security/dependencies.py

echo
echo "security-47 summary: $passed/$total suites passed"
if ((${#failures[@]})); then
    echo "security-47 failures:"
    for failure in "${failures[@]}"; do
        echo "  - $failure"
    done
    exit 1
fi
echo "security-47 result: PASS"
