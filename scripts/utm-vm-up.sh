#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

VM_HOST="${VM_HOST:-}"
VM_USER="${VM_USER:-ubuntu}"
VM_PORT="${VM_PORT:-}"
VM_DIR="${VM_DIR:-/home/${VM_USER}/nostr-vpn}"
BUILD_PROFILE="${BUILD_PROFILE:-debug}"
NETWORK_ID="${NETWORK_ID:-utm-host-vm}"
HOST_CONFIG="${HOST_CONFIG:-}"
VM_CONFIG="${VM_CONFIG:-/home/${VM_USER}/.config/nvpn/config.toml}"
RUN_LOCAL_SCRIPT="${RUN_LOCAL_SCRIPT:-}"
RUN_REMOTE_SCRIPT="${RUN_REMOTE_SCRIPT:-}"

DEPLOY_SCRIPT="$ROOT_DIR/scripts/utm-vm-deploy.sh"
NETWORK_SCRIPT="$ROOT_DIR/scripts/utm-vm-network.sh"

log() {
  printf '[up] %s\n' "$*" >&2
}

die() {
  printf '[up] error: %s\n' "$*" >&2
  exit 1
}

build_flag() {
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    printf '%s' '--release'
  fi
}

local_nvpn_path() {
  local profile_dir="debug"
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    profile_dir="release"
  fi
  printf '%s/target/%s/nvpn' "$ROOT_DIR" "$profile_dir"
}

default_host_config() {
  case "$(uname -s)" in
    Darwin)
      printf '%s/Library/Application Support/nvpn/config.toml' "$HOME"
      ;;
    *)
      printf '%s/.config/nvpn/config.toml' "$HOME"
      ;;
  esac
}

ssh_connect_cmd() {
  if [[ -n "$VM_PORT" ]]; then
    printf 'ssh -p %s "%s@%s"' "$VM_PORT" "$VM_USER" "$VM_HOST"
  else
    printf 'ssh "%s@%s"' "$VM_USER" "$VM_HOST"
  fi
}

require_inputs() {
  [[ -n "$VM_HOST" ]] || die "set VM_HOST"
  [[ -x "$DEPLOY_SCRIPT" ]] || die "deploy script is not executable: $DEPLOY_SCRIPT"
  [[ -x "$NETWORK_SCRIPT" ]] || die "network script is not executable: $NETWORK_SCRIPT"

  if [[ -z "$HOST_CONFIG" ]]; then
    HOST_CONFIG="$(default_host_config)"
  fi
}

build_local() {
  local cmd
  cmd=(cargo build -p nostr-vpn-cli -p nostr-vpn-relay)
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    cmd+=(--release)
  fi

  log "building locally"
  (
    cd "$ROOT_DIR"
    "${cmd[@]}"
  )
}

run_deploy() {
  log "deploying to VM and building remotely"
  VM_HOST="$VM_HOST" \
  VM_USER="$VM_USER" \
  VM_PORT="$VM_PORT" \
  VM_DIR="$VM_DIR" \
  BUILD_PROFILE="$BUILD_PROFILE" \
  RUN_LOCAL_SCRIPT="$RUN_LOCAL_SCRIPT" \
  RUN_REMOTE_SCRIPT="$RUN_REMOTE_SCRIPT" \
  "$DEPLOY_SCRIPT"
}

run_network() {
  local host_nvpn="$1"
  local vm_nvpn="$2"

  log "configuring host and VM network"
  VM_HOST="$VM_HOST" \
  VM_USER="$VM_USER" \
  VM_PORT="$VM_PORT" \
  NETWORK_ID="$NETWORK_ID" \
  HOST_CONFIG="$HOST_CONFIG" \
  VM_CONFIG="$VM_CONFIG" \
  HOST_NVPN="$host_nvpn" \
  VM_NVPN="$vm_nvpn" \
  "$NETWORK_SCRIPT"
}

summarize() {
  local host_nvpn="$1"
  local vm_nvpn="$2"
  local ssh_cmd
  ssh_cmd="$(ssh_connect_cmd)"

  cat <<EOF

UTM deploy and network setup complete.

Local nvpn: $host_nvpn
VM nvpn: $vm_nvpn
Host config: $HOST_CONFIG
VM config: $VM_CONFIG
Network ID: $NETWORK_ID

Next commands:
  sudo "$host_nvpn" connect --config "$HOST_CONFIG"
  $ssh_cmd "sudo \"$vm_nvpn\" connect --config \"$VM_CONFIG\""
  "$host_nvpn" status --config "$HOST_CONFIG" --json
  $ssh_cmd "\"$vm_nvpn\" status --config \"$VM_CONFIG\" --json"
EOF
}

main() {
  require_inputs
  build_local

  local host_nvpn vm_nvpn
  host_nvpn="$(local_nvpn_path)"
  [[ -x "$host_nvpn" ]] || die "local nvpn binary was not built: $host_nvpn"

  vm_nvpn="$(run_deploy)"
  [[ -n "$vm_nvpn" ]] || die "deploy script did not return a VM nvpn path"

  run_network "$host_nvpn" "$vm_nvpn"
  summarize "$host_nvpn" "$vm_nvpn"
}

main "$@"
