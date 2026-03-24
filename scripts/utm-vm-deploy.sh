#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

VM_HOST="${VM_HOST:-}"
VM_USER="${VM_USER:-ubuntu}"
VM_PORT="${VM_PORT:-}"
VM_DIR="${VM_DIR:-/home/${VM_USER}/nostr-vpn}"
BUILD_PROFILE="${BUILD_PROFILE:-debug}"
RUN_LOCAL_SCRIPT="${RUN_LOCAL_SCRIPT:-}"
RUN_REMOTE_SCRIPT="${RUN_REMOTE_SCRIPT:-}"

SSH_TARGET="${VM_USER}@${VM_HOST}"
SSH_OPTS=(-o BatchMode=yes -o StrictHostKeyChecking=accept-new)

if [[ -n "$VM_PORT" ]]; then
  SSH_OPTS=(-p "$VM_PORT" "${SSH_OPTS[@]}")
  RSYNC_RSH="ssh -p ${VM_PORT} -o BatchMode=yes -o StrictHostKeyChecking=accept-new"
else
  RSYNC_RSH="ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new"
fi

log() {
  printf '[deploy] %s\n' "$*" >&2
}

die() {
  printf '[deploy] error: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

build_flag() {
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    printf '%s' '--release'
  fi
}

vm_nvpn_path() {
  local profile_dir="debug"
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    profile_dir="release"
  fi
  printf '%s/target/%s/nvpn' "$VM_DIR" "$profile_dir"
}

ssh_run() {
  ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "$@"
}

remote_login_shell() {
  local cmd="$1"
  ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "bash -lc $(printf '%q' "$cmd")"
}

ensure_local_prereqs() {
  need_cmd ssh
  need_cmd rsync
}

ensure_remote_prereqs() {
  log "checking remote tooling"
  remote_login_shell "command -v cargo >/dev/null 2>&1 || { echo 'cargo is not installed on the VM' >&2; exit 1; }"
}

rsync_repo() {
  log "syncing repo to ${SSH_TARGET}:${VM_DIR}"
  ssh_run mkdir -p "$VM_DIR"
  rsync -az --delete \
    --exclude '.git' \
    --exclude 'target' \
    --exclude 'target-linux' \
    --exclude 'node_modules' \
    --exclude 'dist' \
    -e "$RSYNC_RSH" \
    "$ROOT_DIR/" "${SSH_TARGET}:${VM_DIR}/"
}

build_remote() {
  local cmd
  cmd="cd $(printf '%q' "$VM_DIR") && cargo build -p nostr-vpn-cli -p nostr-vpn-relay"
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    cmd+=" --release"
  fi

  log "building on VM"
  remote_login_shell "$cmd"
}

run_requested_scripts() {
  if [[ -n "$RUN_LOCAL_SCRIPT" ]]; then
    log "running local script: ${RUN_LOCAL_SCRIPT}"
    (cd "$ROOT_DIR" && bash -lc "$RUN_LOCAL_SCRIPT")
  fi

  if [[ -n "$RUN_REMOTE_SCRIPT" ]]; then
    log "running remote script: ${RUN_REMOTE_SCRIPT}"
    remote_login_shell "cd $(printf '%q' "$VM_DIR") && $RUN_REMOTE_SCRIPT"
  fi
}

main() {
  [[ -n "$VM_HOST" ]] || die "set VM_HOST to the Ubuntu VM hostname or IP"

  ensure_local_prereqs
  ensure_remote_prereqs
  rsync_repo
  build_remote
  run_requested_scripts
  vm_nvpn_path
}

main "$@"
