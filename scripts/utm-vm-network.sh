#!/usr/bin/env bash
set -euo pipefail

VM_HOST="${VM_HOST:-}"
VM_USER="${VM_USER:-ubuntu}"
VM_PORT="${VM_PORT:-}"
NETWORK_ID="${NETWORK_ID:-utm-host-vm}"
HOST_CONFIG="${HOST_CONFIG:-}"
VM_CONFIG="${VM_CONFIG:-/home/${VM_USER}/.config/nvpn/config.toml}"
HOST_NVPN="${HOST_NVPN:-}"
VM_NVPN="${VM_NVPN:-}"

SSH_TARGET="${VM_USER}@${VM_HOST}"
SSH_OPTS=(-o BatchMode=yes -o StrictHostKeyChecking=accept-new)

if [[ -n "$VM_PORT" ]]; then
  SSH_OPTS=(-p "$VM_PORT" "${SSH_OPTS[@]}")
fi

log() {
  printf '[network] %s\n' "$*"
}

die() {
  printf '[network] error: %s\n' "$*" >&2
  exit 1
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

ssh_run() {
  ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "$@"
}

remote_login_shell() {
  local cmd="$1"
  ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "bash -lc $(printf '%q' "$cmd")"
}

require_inputs() {
  [[ -n "$VM_HOST" ]] || die "set VM_HOST"
  [[ -n "$HOST_NVPN" ]] || die "set HOST_NVPN to the local nvpn binary path"
  [[ -n "$VM_NVPN" ]] || die "set VM_NVPN to the VM nvpn binary path"

  if [[ -z "$HOST_CONFIG" ]]; then
    HOST_CONFIG="$(default_host_config)"
  fi
}

ensure_prereqs() {
  command -v ssh >/dev/null 2>&1 || die "missing required command: ssh"
  [[ -x "$HOST_NVPN" ]] || die "local nvpn binary is not executable: $HOST_NVPN"
  log "checking remote nvpn"
  remote_login_shell "test -x $(printf '%q' "$VM_NVPN") || { echo 'nvpn binary is not executable on the VM: $VM_NVPN' >&2; exit 1; }"
}

init_configs() {
  log "initializing local config"
  mkdir -p "$(dirname "$HOST_CONFIG")"
  "$HOST_NVPN" init --config "$HOST_CONFIG" --force >/dev/null

  log "initializing VM config"
  remote_login_shell "mkdir -p $(printf '%q' "$(dirname "$VM_CONFIG")") && $(printf '%q' "$VM_NVPN") init --config $(printf '%q' "$VM_CONFIG") --force >/dev/null"
}

extract_local_npub() {
  awk -F'"' '/^public_key/ {print $2; exit}' "$HOST_CONFIG"
}

extract_remote_npub() {
  remote_login_shell "awk -F'\"' '/^public_key/ {print \$2; exit}' $(printf '%q' "$VM_CONFIG")"
}

configure_local() {
  local host_npub="$1"
  local vm_npub="$2"

  log "writing local network config"
  "$HOST_NVPN" set \
    --config "$HOST_CONFIG" \
    --network-id "$NETWORK_ID" \
    --participant "$host_npub" \
    --participant "$vm_npub" >/dev/null
}

configure_remote() {
  local host_npub="$1"
  local vm_npub="$2"

  log "writing VM network config"
  remote_login_shell "$(printf '%q' "$VM_NVPN") set --config $(printf '%q' "$VM_CONFIG") --network-id $(printf '%q' "$NETWORK_ID") --participant $(printf '%q' "$host_npub") --participant $(printf '%q' "$vm_npub") >/dev/null"
}

summarize() {
  local host_npub="$1"
  local vm_npub="$2"
  local ssh_cmd="ssh"
  local ssh_tty_cmd="ssh -t"
  local remote_connect
  local remote_status

  if [[ -n "$VM_PORT" ]]; then
    ssh_cmd+=" -p $VM_PORT"
    ssh_tty_cmd+=" -p $VM_PORT"
  fi

  remote_connect="$(printf 'bash -lc %q' "sudo $(printf '%q' "$VM_NVPN") connect --config $(printf '%q' "$VM_CONFIG")")"
  remote_status="$(printf 'bash -lc %q' "$(printf '%q' "$VM_NVPN") status --config $(printf '%q' "$VM_CONFIG") --json")"

  cat <<EOF

Network config written.

Host config: $HOST_CONFIG
VM config: $VM_CONFIG
Network ID: $NETWORK_ID
Host npub: $host_npub
VM npub: $vm_npub

Next commands:
  sudo "$HOST_NVPN" connect --config "$HOST_CONFIG"
  ${ssh_tty_cmd} "$SSH_TARGET" "$remote_connect"
  "$HOST_NVPN" status --config "$HOST_CONFIG" --json
  ${ssh_cmd} "$SSH_TARGET" "$remote_status"
EOF
}

main() {
  require_inputs
  ensure_prereqs
  init_configs

  local host_npub vm_npub
  host_npub="$(extract_local_npub)"
  vm_npub="$(extract_remote_npub)"

  [[ -n "$host_npub" ]] || die "failed to extract host public_key from $HOST_CONFIG"
  [[ -n "$vm_npub" ]] || die "failed to extract VM public_key from $VM_CONFIG"

  configure_local "$host_npub" "$vm_npub"
  configure_remote "$host_npub" "$vm_npub"
  summarize "$host_npub" "$vm_npub"
}

main "$@"
