#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

VM_HOST="${VM_HOST:-}"
VM_USER="${VM_USER:-ubuntu}"
VM_PORT="${VM_PORT:-22}"
VM_DIR="${VM_DIR:-/home/${VM_USER}/nostr-vpn}"
BUILD_PROFILE="${BUILD_PROFILE:-debug}"
REMOTE_SUDO="${REMOTE_SUDO:-sudo}"
REMOTE_BOOTSTRAP="${REMOTE_BOOTSTRAP:-1}"
SYNC_KEYS="${SYNC_KEYS:-1}"
RUN_LOCAL_SCRIPT="${RUN_LOCAL_SCRIPT:-}"
RUN_REMOTE_SCRIPT="${RUN_REMOTE_SCRIPT:-}"
LOCAL_SSH_PUBKEY="${LOCAL_SSH_PUBKEY:-$HOME/.ssh/id_ed25519.pub}"
REMOTE_SSH_PUBKEY="${REMOTE_SSH_PUBKEY:-/home/${VM_USER}/.ssh/id_ed25519.pub}"

SSH_TARGET="${VM_USER}@${VM_HOST}"
SSH_OPTS=(-p "$VM_PORT" -o BatchMode=yes -o StrictHostKeyChecking=accept-new)
RSYNC_RSH="ssh -p ${VM_PORT} -o BatchMode=yes -o StrictHostKeyChecking=accept-new"

log() {
  printf '[deploy] %s\n' "$*"
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

ssh_run() {
  ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "$@"
}

ensure_local_prereqs() {
  need_cmd ssh
  need_cmd rsync
  need_cmd ssh-keygen
}

ensure_remote_prereqs() {
  log "checking remote build prerequisites"
  ssh "${SSH_OPTS[@]}" "$SSH_TARGET" bash -s -- "$REMOTE_BOOTSTRAP" "$REMOTE_SUDO" <<'EOF'
set -euo pipefail

bootstrap="$1"
remote_sudo="$2"

if command -v apt-get >/dev/null 2>&1; then
  "$remote_sudo" apt-get update
  "$remote_sudo" apt-get install -y curl ca-certificates build-essential pkg-config libssl-dev
fi

if command -v cargo >/dev/null 2>&1; then
  exit 0
fi

if [[ "$bootstrap" != "1" ]]; then
  echo "cargo is not installed on the VM and REMOTE_BOOTSTRAP=0" >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required to bootstrap rustup on the VM" >&2
  exit 1
fi

curl https://sh.rustup.rs -sSf | sh -s -- -y
EOF
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
  log "building on VM"
  ssh "${SSH_OPTS[@]}" "$SSH_TARGET" bash -lc \
    "source \"\$HOME/.cargo/env\" 2>/dev/null || true; cd $(printf '%q' "$VM_DIR"); cargo build $(build_flag)"
}

ensure_local_ssh_key() {
  if [[ ! -f "$LOCAL_SSH_PUBKEY" ]]; then
    log "creating local SSH key at ${LOCAL_SSH_PUBKEY%*.pub}"
    mkdir -p "$(dirname "$LOCAL_SSH_PUBKEY")"
    ssh-keygen -t ed25519 -N '' -f "${LOCAL_SSH_PUBKEY%.pub}" >/dev/null
  fi
}

ensure_remote_ssh_key() {
  ssh "${SSH_OPTS[@]}" "$SSH_TARGET" bash -lc \
    "mkdir -p ~/.ssh && chmod 700 ~/.ssh && if [ ! -f $(printf '%q' "$REMOTE_SSH_PUBKEY") ]; then ssh-keygen -t ed25519 -N '' -f $(printf '%q' "${REMOTE_SSH_PUBKEY%.pub}") >/dev/null; fi"
}

sync_keys() {
  log "syncing SSH authorized_keys between host and VM"
  ensure_local_ssh_key
  ensure_remote_ssh_key

  local local_pubkey remote_pubkey
  local_pubkey="$(cat "$LOCAL_SSH_PUBKEY")"
  remote_pubkey="$(ssh "${SSH_OPTS[@]}" "$SSH_TARGET" cat "$REMOTE_SSH_PUBKEY")"

  ssh "${SSH_OPTS[@]}" "$SSH_TARGET" bash -lc \
    "mkdir -p ~/.ssh && chmod 700 ~/.ssh && touch ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && grep -Fqx $(printf '%q' "$local_pubkey") ~/.ssh/authorized_keys || printf '%s\n' $(printf '%q' "$local_pubkey") >> ~/.ssh/authorized_keys"

  mkdir -p "$HOME/.ssh"
  touch "$HOME/.ssh/authorized_keys"
  chmod 700 "$HOME/.ssh"
  chmod 600 "$HOME/.ssh/authorized_keys"
  if ! grep -Fqx "$remote_pubkey" "$HOME/.ssh/authorized_keys"; then
    printf '%s\n' "$remote_pubkey" >>"$HOME/.ssh/authorized_keys"
  fi
}

run_requested_scripts() {
  if [[ -n "$RUN_LOCAL_SCRIPT" ]]; then
    log "running local script: ${RUN_LOCAL_SCRIPT}"
    (cd "$ROOT_DIR" && bash -lc "$RUN_LOCAL_SCRIPT")
  fi

  if [[ -n "$RUN_REMOTE_SCRIPT" ]]; then
    log "running remote script: ${RUN_REMOTE_SCRIPT}"
    ssh "${SSH_OPTS[@]}" "$SSH_TARGET" bash -lc \
      "source \"\$HOME/.cargo/env\" 2>/dev/null || true; cd $(printf '%q' "$VM_DIR"); $RUN_REMOTE_SCRIPT"
  fi
}

summarize() {
  cat <<EOF

Deployment complete.

VM:
  repo: $VM_DIR
  build: cargo build ${BUILD_PROFILE:+($BUILD_PROFILE)}

Suggested next commands:
  SSH into VM: ssh -p "$VM_PORT" "$SSH_TARGET"
  Rebuild on VM: ssh -p "$VM_PORT" "$SSH_TARGET" 'cd "$VM_DIR" && . "$HOME/.cargo/env" 2>/dev/null || true && cargo build $(build_flag)'
EOF
}

main() {
  [[ -n "$VM_HOST" ]] || die "set VM_HOST to the Ubuntu VM hostname or IP"

  ensure_local_prereqs
  ensure_remote_prereqs
  rsync_repo
  build_remote

  if [[ "$SYNC_KEYS" == "1" ]]; then
    sync_keys
  fi

  run_requested_scripts
  summarize
}

main "$@"
