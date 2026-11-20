#!/usr/bin/env bash
# Local development orchestration for Gateway, Worker Packages, and CLI.
#
#   scripts/dev.sh start gateway
#   scripts/dev.sh start gb-client
#   scripts/dev.sh start worker <general|coding|knowledge>
#   scripts/dev.sh stop gateway|worker <name>|all
#   scripts/dev.sh status
#
# State and logs live under .goldenbee/dev (git-ignored). Tokens are generated
# once and remain stable for this checkout.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEV_DIR="$ROOT/.goldenbee/dev"
BIN_DIR="$DEV_DIR/bin"
RUN_DIR="$DEV_DIR/run"
WORKER_DIR="$DEV_DIR/workers"
CLI_HOME="$DEV_DIR/cli"
GATEWAY_ADDR="${GATEWAY_ADDR:-127.0.0.1:18080}"
BASE_WORKER_PORT="${BASE_WORKER_PORT:-18081}"
CALLER_TOKEN_FILE="$DEV_DIR/caller-token"
DELEGATION_SECRET_FILE="$DEV_DIR/delegation-secret"
VAULT_MASTER_KEY_FILE="$DEV_DIR/vault-master-key"

# Local secrets are loaded from the ignored .env, never printed or copied into
# generated configuration. Existing process variables win.
if [[ -f "$ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  . "$ROOT/.env"
  set +a
fi

mkdir -p "$BIN_DIR" "$RUN_DIR" "$WORKER_DIR" "$CLI_HOME"
chmod 700 "$DEV_DIR" "$RUN_DIR" "$WORKER_DIR" "$CLI_HOME"
log() { printf 'dev: %s\n' "$*"; }
die() { printf 'dev: %s\n' "$*" >&2; exit 1; }
wait_http() {
  local url="$1" pid="$2" name="$3"
  for _ in $(seq 1 120); do
    curl -fsS "$url" >/dev/null 2>&1 && return 0
    kill -0 "$pid" 2>/dev/null || die "$name exited; see $DEV_DIR/$name.log"
    sleep 0.1
  done
  die "$name did not become healthy at $url"
}
token() {
  local file="$1"
  if [[ ! -s "$file" ]]; then
    if command -v openssl >/dev/null 2>&1; then
      openssl rand -hex 24 >"$file"
    else
      head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' >"$file"
    fi
    chmod 600 "$file"
  fi
  cat "$file"
}
vault_master_key() {
  if [[ ! -s "$VAULT_MASTER_KEY_FILE" ]]; then
    head -c 32 /dev/urandom | base64 | tr -d '\n' >"$VAULT_MASTER_KEY_FILE"
    chmod 600 "$VAULT_MASTER_KEY_FILE"
  fi
  cat "$VAULT_MASTER_KEY_FILE"
}
provider_vault_enabled() {
  [[ -n "${OPENROUTER_API_KEY:-}" && -n "${OPENROUTER_MODEL_KEY:-}" ]]
}
pidfile() { printf '%s/%s.pid' "$RUN_DIR" "$1"; }
stop_pidfile() {
  local name="$1" file
  file="$(pidfile "$name")"
  [[ -f "$file" ]] || return 0
  local pid
  pid="$(cat "$file" 2>/dev/null || true)"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 120); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.1
    done
    kill -KILL "$pid" 2>/dev/null || true
  fi
  rm -f "$file"
}
worker_port() {
  case "$1" in
    general) echo "$BASE_WORKER_PORT" ;;
    coding) echo "$((BASE_WORKER_PORT + 1))" ;;
    knowledge) echo "$((BASE_WORKER_PORT + 2))" ;;
    *) die "unsupported worker '$1' (use general, coding, or knowledge)" ;;
  esac
}
worker_id() { echo "$1-a"; }
worker_config() { printf '%s/%s.toml' "$WORKER_DIR" "$1"; }
worker_state() { printf '%s/%s-state' "$WORKER_DIR" "$1"; }

install_binary() {
  local source="$1" destination="$2" temporary
  temporary="$destination.tmp.$$"
  cp "$source" "$temporary"
  chmod +x "$temporary"
  # Rename over a running executable instead of truncating it. Linux keeps the
  # old inode alive for the running process and new starts use the new binary.
  mv -f "$temporary" "$destination"
}

ensure_binaries() {
  # `cargo run` is allowed; `cargo build` is not. These warm/invoke the exact
  # binaries without a build-only command. Always invoke them so source changes
  # are rebuilt before a start/restart.
  if [[ "${BINARIES_READY:-0}" == 1 ]]; then return 0; fi
  log "preparing development binaries"
  (cd "$ROOT" && cargo run --locked -p goldenbee-gateway --bin gb-gateway -- --version >/dev/null)
  (cd "$ROOT" && cargo run --locked -p general --bin general -- --version >/dev/null)
  (cd "$ROOT" && cargo run --locked -p goldenbee-client-cli -- --version >/dev/null)
  install_binary "$ROOT/target/debug/gb-gateway" "$BIN_DIR/gb-gateway"
  install_binary "$ROOT/target/debug/general" "$BIN_DIR/general"
  install_binary "$ROOT/target/debug/gb-client" "$BIN_DIR/gb-client"
  install_binary "$ROOT/target/debug/gb" "$BIN_DIR/gb"
  for worker in coding knowledge; do
    (cd "$ROOT" && cargo run --locked -p "$worker" -- --version >/dev/null)
    install_binary "$ROOT/target/debug/$worker" "$BIN_DIR/$worker"
  done
  BINARIES_READY=1
}

prepare_assets() {
  local name="$1" model="$2" source="$ROOT/config/$1" destination="$WORKER_DIR/$1-assets"
  if [[ ! -d "$source" ]]; then
    die "missing development assets for $name at $source"
  fi
  rm -rf "$destination"
  cp -R "$source" "$destination"
  # State belongs to the worker storage directory, never to copied assets.
  # A stale profile there can bind an agent to an old model provider.
  rm -rf "$destination/state"
  if [[ "$model" != "deterministic" ]]; then
    find "$destination/agents" -name '*.toml' -type f -exec \
      sed -i "s/^model = \"deterministic\"/model = \"$model\"/" {} +
  fi
  printf '%s' "$destination"
}

write_worker_config() {
  local name="$1" config id port src assets
  config="$(worker_config "$name")"
  id="$(worker_id "$name")"
  port="$(worker_port "$name")"
  local default_model="deterministic"
  local allow_remote="false"
  local provider_block=""
  local fallback="deterministic"
  if provider_vault_enabled; then
    # Gateway-managed mode: no per-Worker provider block and no provider API key
    # in this file. The Worker fetches an encrypted envelope after enrollment.
    default_model="openrouter"
    allow_remote="true"
    fallback="deny"
  fi
  local models_block
  models_block=$(cat <<EOF

[models]
source = "gateway"
gateway_url = "http://$GATEWAY_ADDR"
control_token = "$(token "$DEV_DIR/worker-$name-token")"
worker_id = "$id"
refresh_ttl_ms = 60000
fallback = "$fallback"
EOF
)
  assets="$(prepare_assets "$name" "$default_model")"
  case "$name" in
    general)
      cat >"$config" <<EOF
instance = "$id"
namespace = "local"
default_agent = "general"
token = "$(token "$DEV_DIR/worker-token")"
addr = "127.0.0.1:$port"
default_model = "$default_model"
allow_remote = $allow_remote
[paths]
agents = "$assets/agents"
prompts = "$assets/prompts"
skills = "$assets/skills"
[storage]
directory = "$(worker_state "$name")"
[settings]
mode = "local"
EOF
      ;;
    coding)
      src="$ROOT/config/coding.toml"
      cat >"$config" <<EOF
instance = "$id"
namespace = "local"
default_agent = "coder"
token = "$(token "$DEV_DIR/worker-token")"
token_scopes = ["*"]
addr = "127.0.0.1:$port"
default_model = "$default_model"
allow_remote = $allow_remote
[paths]
agents = "$assets/agents"
prompts = "$assets/prompts"
skills = "$assets/skills"
[storage]
directory = "$(worker_state "$name")"
[settings]
workspace_root = "$ROOT/.goldenbee/dev/coding-workspace"
repo = "repo"
base_ref = "HEAD"
checks = "sh tests/check.sh, sh tests/lint.sh"
developer_model = "deterministic"
reviewer_model = "deterministic"
build_parallelism = "1"
max_build_seconds = "120"
max_output_bytes = "262144"
max_memory_bytes = "1073741824"
EOF
      ;;
    knowledge)
      cat >"$config" <<EOF
instance = "$id"
namespace = "local"
default_agent = "knowledge"
token = "$(token "$DEV_DIR/worker-token")"
token_scopes = ["*"]
addr = "127.0.0.1:$port"
default_model = "$default_model"
allow_remote = $allow_remote
[paths]
agents = "$assets/agents"
prompts = "$assets/prompts"
skills = "$assets/skills"
[storage]
directory = "$(worker_state "$name")"
[settings]
source_roots = "$ROOT/config/knowledge/sources"
max_document_bytes = "16777216"
chunk_size = "512"
max_chunks = "10000"
top_k = "5"
max_top_k = "20"
EOF
      ;;
  esac
  if [[ -n "$provider_block" ]]; then
    printf '%s\n' "$provider_block" >>"$config"
  fi
  if [[ -n "$models_block" ]]; then
    printf '%s\n' "$models_block" >>"$config"
  fi
  cat >>"$config" <<EOF

[gateway]
url = "http://$GATEWAY_ADDR"
control_token = "$(token "$DEV_DIR/worker-$name-token")"
advertise_url = "http://127.0.0.1:$port"
renew_interval_ms = 5000
EOF
  chmod 600 "$config"
}

ensure_coding_workspace() {
  local repo="$ROOT/.goldenbee/dev/coding-workspace/repo"
  if [[ ! -d "$repo/.git" ]]; then
    mkdir -p "$repo"
    git -C "$repo" init -q
    printf '#!/bin/sh\nexit 0\n' >"$repo/tests.sh"
    git -C "$repo" add tests.sh
    git -C "$repo" -c user.email=dev@example.invalid -c user.name=dev commit -qm fixture
  fi
  mkdir -p "$repo/tests"
  printf '#!/bin/sh\nexit 0\n' >"$repo/tests/check.sh"
  printf '#!/bin/sh\nexit 0\n' >"$repo/tests/lint.sh"
  chmod +x "$repo/tests/check.sh" "$repo/tests/lint.sh"
}

setup() {
  ensure_binaries
  # Setup is an explicit local reset: stale approvals bound to old generated
  # control tokens must not survive a fresh workspace.
  rm -rf "$DEV_DIR/gateway-state" "$DEV_DIR/gateway-registry.json" "$DEV_DIR/vault"
  rm -rf "$WORKER_DIR"/general-state "$WORKER_DIR"/coding-state "$WORKER_DIR"/knowledge-state
  vault_master_key >/dev/null
  rm -rf "$CLI_HOME"
  mkdir -p "$CLI_HOME"
  chmod 700 "$CLI_HOME"
  local name
  for name in general coding knowledge; do
    write_worker_config "$name"
  done
  ensure_coding_workspace
  token "$CALLER_TOKEN_FILE" >/dev/null
  for name in general coding knowledge; do token "$DEV_DIR/worker-$name-token" >/dev/null; done
  token "$DELEGATION_SECRET_FILE" >/dev/null
  log "development state ready at $DEV_DIR"
  log "next: make start gateway"
  log "then: make start gb-client"
}

start_worker() {
  local name="${1:-}" config id port
  [[ -n "$name" ]] || die 'usage: scripts/dev.sh start worker <general|coding|knowledge>'
  ensure_binaries
  config="$(worker_config "$name")"
  id="$(worker_id "$name")"
  port="$(worker_port "$name")"
  write_worker_config "$name"
  if [[ -f "$(pidfile "worker-$name")" ]]; then
    local old
    old="$(cat "$(pidfile "worker-$name")")"
    if kill -0 "$old" 2>/dev/null; then log "worker $name already running (pid $old)"; return 0; fi
    rm -f "$(pidfile "worker-$name")"
  fi
  if [[ "$name" == "coding" ]]; then
    ensure_coding_workspace
  fi
  log "starting worker $name on 127.0.0.1:$port"
  (
    cd "$ROOT"
    export GOLDENBEE_DELEGATION_SECRET="$(token "$DELEGATION_SECRET_FILE")"
    # Development web UI; production must not set this.
    export GOLDENBEE_DEV_UI=1
    exec setsid "$BIN_DIR/$name" --config "$config" serve --addr "127.0.0.1:$port"
  ) </dev/null >"$DEV_DIR/worker-$name.log" 2>&1 &
  local pid=$!
  echo "$pid" >"$(pidfile "worker-$name")"
  mkdir -p "$(worker_state "$name")"
  echo "$pid" >"$(worker_state "$name")/worker.pid"
  wait_http "http://127.0.0.1:$port/v1/health" "$pid" "worker-$name"
  log "worker $name ready"
}

running_workers() {
  local name
  for name in general coding knowledge; do
    local file pid
    file="$(pidfile "worker-$name")"
    [[ -f "$file" ]] || continue
    pid="$(cat "$file" 2>/dev/null || true)"
    kill -0 "$pid" 2>/dev/null && echo "$name"
  done
}

write_gateway_config() {
  local config="$DEV_DIR/gateway.json" caller first=1
  caller="$(token "$CALLER_TOKEN_FILE")"
  {
    printf '{\n  "listen": "%s",\n  "data_dir": "%s",\n  "registry": "%s",\n' \
      "$GATEWAY_ADDR" "$DEV_DIR/gateway-state" "$DEV_DIR/gateway-registry.json"
    printf '  "lease_ttl_ms": 30000,\n'
    printf '  "vault": {"enabled": true, "directory": "%s", "master_key_env": "GOLDENBEE_VAULT_MASTER_KEY"},\n' "$DEV_DIR/vault"
    printf '  "callers": [\n'
    # Local development has no user management or business roles. The single
    # dev caller is unrestricted so every CLI and TUI operation is available
    # from the active `local` profile.
    printf '    {"token": "%s", "id": "local", "namespace": "local", "scopes": ["*"]}\n' "$caller"
    printf '  ],\n  "workers": [\n'
    first=1
    for name in general coding knowledge; do
      [[ -f "$(worker_config "$name")" ]] || continue
      local port
      port="$(worker_port "$name")"
      [[ "$first" == 1 ]] || printf ',\n'
      printf '    {"id": "%s", "endpoint": "http://127.0.0.1:%s", "instance": "%s", "package": "%s", "capabilities": ["%s"], "control_token": "%s", "preflight": false}' \
        "$(worker_id "$name")" "$port" "$(worker_id "$name")" "$name" "$(worker_config_agent "$name")" "$(token "$DEV_DIR/worker-$name-token")"
      first=0
    done
    printf '\n  ]\n}\n'
  } >"$config"
  chmod 600 "$config"
}

worker_config_agent() {
  case "$1" in general) echo general;; coding) echo coder;; knowledge) echo knowledge;; esac
}

configure_vault_provider() {
  provider_vault_enabled || return 0
  local caller url provider_workers=()
  caller="$(token "$CALLER_TOKEN_FILE")"
  url="http://$GATEWAY_ADDR"
  local name model_args=()
  for name in general coding knowledge; do
    provider_workers+=(--worker "$(worker_id "$name")")
  done
  if [[ -n "${OPENROUTER_MODEL_KEY:-}" ]]; then
    model_args=(--model "$OPENROUTER_MODEL_KEY")
  else
    # Explicit development policy: select the deterministic first discovered
    # model. Production must set a preferred model instead.
    model_args=(--model "" --auto-default)
  fi
  # The provider key enters the Gateway only through credential set's stdin.
  # It is never an argument, a generated Worker config, or a log line.
  local revision
  revision="$("$BIN_DIR/gb-gateway" provider list \
    --url "$url" --token "$caller" --json 2>/dev/null \
    | python3 -c 'import json,sys
providers=json.load(sys.stdin)
match=[p for p in providers if p.get("id")=="openrouter"]
print(match[0]["revision"] if match else "")' 2>/dev/null || true)"
  if [[ -n "$revision" ]]; then
    "$BIN_DIR/gb-gateway" provider update \
      --url "$url" --token "$caller" --revision "$revision" \
      --id openrouter --adapter openrouter "${model_args[@]}" \
      --endpoint "https://openrouter.ai/api/v1" --modality text --tool-calling \
      --context-window 128000 --residency remote \
      "${provider_workers[@]}" >/dev/null
  else
    "$BIN_DIR/gb-gateway" provider add \
      --url "$url" --token "$caller" \
      --id openrouter --adapter openrouter "${model_args[@]}" \
      --endpoint "https://openrouter.ai/api/v1" --modality text --tool-calling \
      --context-window 128000 --residency remote \
      "${provider_workers[@]}" >/dev/null
  fi
  local credential_json discovery_error
  credential_json="$(printf '%s' "$OPENROUTER_API_KEY" | "$BIN_DIR/gb-gateway" provider credential set \
    --url "$url" --token "$caller" --id openrouter --json)"
  if ! python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get("ready") else 1)' <<<"$credential_json"; then
    discovery_error="$(python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("discovery_error") or d.get("discovery_error_code") or "discovery failed")' <<<"$credential_json")"
    die "gb-gateway provider discovery failed: $discovery_error"
  fi
  log "gb-gateway provider credential loaded and models discovered"
}

enroll_workers() {
  provider_vault_enabled || return 0
  local caller url name pair_json code
  caller="$(token "$CALLER_TOKEN_FILE")"
  url="http://$GATEWAY_ADDR"
  for name in general coding knowledge; do
    # Regenerate the Worker config before enrollment. A setup created before
    # the provider vault was enabled has no [models] gateway block.
    write_worker_config "$name"
    pair_json="$("$BIN_DIR/gb-gateway" provider pair \
      --url "$url" --token "$caller" \
      --worker "$(worker_id "$name")" --instance "$(worker_id "$name")" --json)"
    code="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["code"])' <<<"$pair_json")"
    "$BIN_DIR/$name" enroll --config "$(worker_config "$name")" --code "$code" >/dev/null
  done
  log "workers enrolled for Gateway provider envelopes"
}

restart_running_workers() {
  provider_vault_enabled || return 0
  local name pid
  for name in general coding knowledge; do
    [[ -f "$(pidfile "worker-$name")" ]] || continue
    pid="$(cat "$(pidfile "worker-$name")" 2>/dev/null || true)"
    kill -0 "$pid" 2>/dev/null || continue
    log "restarting worker $name to apply the Gateway provider configuration"
    stop_pidfile "worker-$name"
    start_worker "$name"
  done
}

start_gateway() {
  ensure_binaries
  if [[ -f "$(pidfile gateway)" ]]; then
    local old
    old="$(cat "$(pidfile gateway)")"
    if kill -0 "$old" 2>/dev/null; then
      log "gateway already running (pid $old)"
      export GOLDENBEE_CLI_HOME="$CLI_HOME"
      "$BIN_DIR/gb-client" gw add local "http://$GATEWAY_ADDR" "$(token "$CALLER_TOKEN_FILE")" >/dev/null
      configure_vault_provider
      enroll_workers
      restart_running_workers
      if [[ -z "$(running_workers)" ]]; then start_worker general; fi
      return 0
    fi
    rm -f "$(pidfile gateway)"
  fi
  write_gateway_config
  # Development approvals are regenerated from the live Worker set. Dispatch
  # state remains durable; only the small dev registry projection is refreshed.
  rm -f "$DEV_DIR/gateway-registry.json"
  log "starting gateway on $GATEWAY_ADDR"
  (
    cd "$ROOT"
    export GOLDENBEE_DELEGATION_SECRET="$(token "$DELEGATION_SECRET_FILE")"
    export GOLDENBEE_VAULT_MASTER_KEY="$(vault_master_key)"
    exec setsid "$BIN_DIR/gb-gateway" serve --config "$DEV_DIR/gateway.json"
  ) </dev/null >"$DEV_DIR/gateway.log" 2>&1 &
  local pid=$!
  echo "$pid" >"$(pidfile gateway)"
  mkdir -p "$DEV_DIR/gateway-state"
  echo "$pid" >"$DEV_DIR/gateway-state/gateway.pid"
  wait_http "http://$GATEWAY_ADDR/v1/health" "$pid" gateway
  export GOLDENBEE_CLI_HOME="$CLI_HOME"
  "$BIN_DIR/gb-client" gw add local "http://$GATEWAY_ADDR" "$(token "$CALLER_TOKEN_FILE")" >/dev/null
  "$BIN_DIR/gb-client" gw switch local >/dev/null
  # Registration and the first lease renewal are asynchronous. Do not report
  # readiness until the caller can actually see every started Worker.
  local expected visible
  expected="$(running_workers | wc -l)"
  for _ in $(seq 1 100); do
    visible="$(GOLDENBEE_CLI_HOME="$CLI_HOME" "$BIN_DIR/gb-client" workers --json 2>/dev/null \
      | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null || echo 0)"
    [[ "$visible" -ge "$expected" ]] && break
    sleep 0.1
  done
  configure_vault_provider
  enroll_workers
  if [[ -z "$(running_workers)" ]]; then
    start_worker general
  fi
  log "gateway ready"
}

start_all() {
  local name
  # The Gateway vault, provider credential, and Worker enrollment must exist
  # before a Gateway-backed Worker starts. Gateway startup starts general once
  # those steps succeed.
  start_gateway
  for name in general coding knowledge; do
    start_worker "$name"
  done
  log "all development services ready"
  log "pair link: $(dev_pair_link)"
}

dev_pair_link() {
  local caller
  caller="$(token "$CALLER_TOKEN_FILE")"
  local mutquery=""
  local name first=1
  for name in general coding knowledge; do
    local port
    port="$(worker_port "$name")"
    [[ "$first" == 1 ]] || mutquery+=","
    mutquery+="$(worker_id "$name")@http://127.0.0.1:$port"
    first=0
  done
  local query
  query="$(python3 - "$GATEWAY_ADDR" "$caller" "$mutquery" <<'PY'
import sys, urllib.parse
gateway, token, workers = sys.argv[1:4]
print(urllib.parse.urlencode({"gateway": f"http://{gateway}", "caller": "local", "token": token, "workers": workers}))
PY
)"
  printf 'goldenbee://pair/dev?%s' "$query"
}

start_cli() {
  ensure_binaries
  [[ -f "$DEV_DIR/gateway.json" ]] || die "gateway is not configured; run: make start gateway"
  export GOLDENBEE_CLI_HOME="$CLI_HOME"
  exec "$BIN_DIR/gb-client" "$@"
}

status() {
  local name
  printf 'gateway: '
  if [[ -f "$(pidfile gateway)" ]] && kill -0 "$(cat "$(pidfile gateway)")" 2>/dev/null; then echo running; else echo stopped; fi
  for name in general coding knowledge; do
    printf 'worker %s: ' "$name"
    if [[ -f "$(pidfile "worker-$name")" ]] && kill -0 "$(cat "$(pidfile "worker-$name")")" 2>/dev/null; then echo running; else echo stopped; fi
  done
}

stop_all() {
  stop_pidfile gateway
  local name
  for name in general coding knowledge; do stop_pidfile "worker-$name"; done
  log "stopped"
}

case "${1:-}" in
  start)
    case "${2:-}" in
      gateway) start_gateway ;;
      gb-client|client-cli) shift 2; start_cli "$@" ;;
      worker) start_worker "${3:-}" ;;
      all) start_all ;;
      *) die 'usage: scripts/dev.sh start all|gateway|gb-client|worker <name>' ;;
    esac
    ;;
  stop)
    case "${2:-}" in
      all|"") stop_all ;;
      worker) stop_pidfile "worker-${3:-}"; log "worker ${3:-} stopped" ;;
      gateway) stop_pidfile gateway; log "gateway stopped" ;;
      *) die 'usage: scripts/dev.sh stop gateway|worker <name>|all' ;;
    esac
    ;;
  setup) setup ;;
  status) status ;;
  *) die 'usage: scripts/dev.sh setup | start all|gateway|gb-client|worker <name> | stop ... | status' ;;
esac
