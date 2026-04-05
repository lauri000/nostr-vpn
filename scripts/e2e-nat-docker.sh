#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PROJECT_NAME="nostr-vpn-e2e-nat"
COMPOSE=(docker compose -p "$PROJECT_NAME" -f "$ROOT_DIR/docker-compose.nat-e2e.yml")

RELAY_URL="ws://203.0.113.2:8080"
REFLECTOR_ADDR="203.0.113.3:3478"
CONFIG_PATH="/root/.config/nvpn/config.toml"

cleanup() {
  "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
  docker network rm \
    "${PROJECT_NAME}_internet" \
    "${PROJECT_NAME}_private-b" >/dev/null 2>&1 || true
  for network in "${PROJECT_NAME}_internet" "${PROJECT_NAME}_private-b"; do
    for _ in $(seq 1 20); do
      docker network inspect "$network" >/dev/null 2>&1 || break
      sleep 1
    done
  done
}

dump_debug() {
  set +e
  echo "nat docker e2e failed, collecting debug output..."
  "${COMPOSE[@]}" ps || true
  for service in relay reflector nat-b node-a node-b; do
    echo "--- logs: $service ---"
    "${COMPOSE[@]}" logs --no-color --tail 120 "$service" || true
  done
  for node in node-a node-b; do
    echo "--- $node status ---"
    "${COMPOSE[@]}" exec -T "$node" nvpn status --json --discover-secs 0 || true
    echo "--- $node daemon.state.json ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "cat /root/.config/nvpn/daemon.state.json 2>/dev/null || true" || true
    echo "--- $node daemon.log ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "tail -n 200 /root/.config/nvpn/daemon.log 2>/dev/null || true" || true
    echo "--- $node routes ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "ip route || true" || true
    echo "--- $node utun100 ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "ip addr show utun100 || true" || true
    echo "--- $node processes ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "ps ax || true" || true
  done
}

on_exit() {
  local exit_code=$?
  if [[ $exit_code -ne 0 ]]; then
    dump_debug
  fi
  cleanup
  exit "$exit_code"
}
trap on_exit EXIT

compact_json() {
  tr -d '\n\r\t '
}

peer_tunnel_ip_from_status() {
  grep -o '"tunnel_ip":"10\.44\.[0-9]\+\.[0-9]\+/32"' | tail -n1 | cut -d '"' -f4 | cut -d/ -f1 || true
}

peer_announced_endpoint_from_status() {
  grep -o '"endpoint":"[^"]*"' | tail -n1 | cut -d '"' -f4 || true
}

cleanup

wait_for_service() {
  local service="$1"
  local container_id=""
  for _ in $(seq 1 30); do
    container_id="$("${COMPOSE[@]}" ps -q "$service" 2>/dev/null || true)"
    if [[ -n "$container_id" ]] \
      && [[ "$(docker inspect -f '{{.State.Running}}' "$container_id" 2>/dev/null || true)" == "true" ]]; then
      return 0
    fi
    sleep 1
  done

  echo "nat docker e2e failed: service '$service' did not reach running state" >&2
  exit 1
}

ping_until_success() {
  local node="$1"
  local target="$2"
  local log_path="$3"
  for _ in $(seq 1 5); do
    if "${COMPOSE[@]}" exec -T "$node" ping -c 3 -W 2 "$target" >"$log_path"; then
      return 0
    fi
    sleep 2
  done

  return 1
}

private_iface_for_ip() {
  local node="$1"
  local cidr="$2"
  "${COMPOSE[@]}" exec -T "$node" sh -lc \
    "ip -o -4 addr show | awk '\$4 == \"$cidr\" { print \$2; exit }'" | tr -d '\r'
}

nostr_pubkey_from_config() {
  local node="$1"
  "${COMPOSE[@]}" exec -T "$node" sh -lc "
    awk '
      /^\\[nostr\\]$/ { in_nostr = 1; next }
      /^\\[/ { in_nostr = 0 }
      in_nostr && /^public_key[[:space:]]*=/ {
        print \$3;
        exit
      }
    ' '$CONFIG_PATH'
  " | tr -d '\r\"'
}

"${COMPOSE[@]}" build >/dev/null
"${COMPOSE[@]}" up -d relay reflector nat-b >/dev/null

for service in relay reflector nat-b; do
  wait_for_service "$service"
done

"${COMPOSE[@]}" up -d node-a node-b >/dev/null

for service in node-a node-b; do
  wait_for_service "$service"
done

NODE_B_PRIVATE_IFACE="$(private_iface_for_ip node-b 172.30.242.3/24)"
[[ -n "$NODE_B_PRIVATE_IFACE" ]]

"${COMPOSE[@]}" exec -T node-b sh -lc \
  "ip route del default >/dev/null 2>&1 || true; ip route add default via 172.30.242.2 dev $NODE_B_PRIVATE_IFACE"

for node in node-a node-b; do
  "${COMPOSE[@]}" exec -T "$node" nvpn init --force >/dev/null
done

ALICE_NPUB="$(nostr_pubkey_from_config node-a)"
BOB_NPUB="$(nostr_pubkey_from_config node-b)"

if [[ -z "$ALICE_NPUB" || -z "$BOB_NPUB" ]]; then
  echo "nat docker e2e failed: unable to resolve node npubs" >&2
  exit 1
fi

"${COMPOSE[@]}" exec -T node-a nvpn set --participant "$BOB_NPUB" --relay "$RELAY_URL" >/dev/null
"${COMPOSE[@]}" exec -T node-b nvpn set --participant "$ALICE_NPUB" --relay "$RELAY_URL" >/dev/null

for node in node-a node-b; do
  "${COMPOSE[@]}" exec -T "$node" sh -lc \
    "sed -i 's|^reflectors = .*|reflectors = [\"$REFLECTOR_ADDR\"]|' '$CONFIG_PATH'"
  "${COMPOSE[@]}" exec -T "$node" sh -lc \
    "sed -i 's|^discovery_timeout_secs = .*|discovery_timeout_secs = 2|' '$CONFIG_PATH'"
done

"${COMPOSE[@]}" exec -T node-a nvpn start --daemon --connect >/dev/null
"${COMPOSE[@]}" exec -T node-b nvpn start --daemon --connect >/dev/null

ALICE_STATUS=""
BOB_STATUS=""
for _ in $(seq 1 60); do
  ALICE_STATUS="$("${COMPOSE[@]}" exec -T node-a nvpn status --json --discover-secs 0 | tr -d '\r')"
  BOB_STATUS="$("${COMPOSE[@]}" exec -T node-b nvpn status --json --discover-secs 0 | tr -d '\r')"
  ALICE_COMPACT="$(printf '%s' "$ALICE_STATUS" | compact_json)"
  BOB_COMPACT="$(printf '%s' "$BOB_STATUS" | compact_json)"
  ALICE_ANNOUNCED_ENDPOINT="$(printf '%s' "$ALICE_COMPACT" | peer_announced_endpoint_from_status)"
  BOB_ANNOUNCED_ENDPOINT="$(printf '%s' "$BOB_COMPACT" | peer_announced_endpoint_from_status)"
  BOB_TUNNEL_IP="$(printf '%s' "$ALICE_COMPACT" | peer_tunnel_ip_from_status)"
  ALICE_TUNNEL_IP="$(printf '%s' "$BOB_COMPACT" | peer_tunnel_ip_from_status)"

  if grep -q '"status_source":"daemon"' <<<"$ALICE_COMPACT" \
    && grep -q '"status_source":"daemon"' <<<"$BOB_COMPACT" \
    && grep -q '"running":true' <<<"$ALICE_COMPACT" \
    && grep -q '"running":true' <<<"$BOB_COMPACT" \
    && [[ "$ALICE_ANNOUNCED_ENDPOINT" == "203.0.113.11:51820" ]] \
    && [[ "$BOB_ANNOUNCED_ENDPOINT" == "203.0.113.10:51820" ]] \
    && [[ -n "$ALICE_ANNOUNCED_ENDPOINT" ]] \
    && [[ -n "$BOB_ANNOUNCED_ENDPOINT" ]] \
    && [[ -n "$ALICE_TUNNEL_IP" ]] \
    && [[ -n "$BOB_TUNNEL_IP" ]]; then
    break
  fi
  sleep 1
done

printf 'ALICE STATUS\n%s\n' "$ALICE_STATUS"
printf 'BOB STATUS\n%s\n' "$BOB_STATUS"

ALICE_COMPACT="$(printf '%s' "$ALICE_STATUS" | compact_json)"
BOB_COMPACT="$(printf '%s' "$BOB_STATUS" | compact_json)"

grep -q '"status_source":"daemon"' <<<"$ALICE_COMPACT"
grep -q '"status_source":"daemon"' <<<"$BOB_COMPACT"
grep -q '"running":true' <<<"$ALICE_COMPACT"
grep -q '"running":true' <<<"$BOB_COMPACT"
ALICE_ANNOUNCED_ENDPOINT="$(printf '%s' "$ALICE_COMPACT" | peer_announced_endpoint_from_status)"
BOB_ANNOUNCED_ENDPOINT="$(printf '%s' "$BOB_COMPACT" | peer_announced_endpoint_from_status)"

if [[ "$ALICE_ANNOUNCED_ENDPOINT" != "203.0.113.11:51820" ]]; then
  echo "nat docker e2e failed: alice did not observe bob's public endpoint announcement ('$ALICE_ANNOUNCED_ENDPOINT')" >&2
  exit 1
fi
if [[ "$BOB_ANNOUNCED_ENDPOINT" != "203.0.113.10:51820" ]]; then
  echo "nat docker e2e failed: bob did not observe alice's public endpoint announcement ('$BOB_ANNOUNCED_ENDPOINT')" >&2
  exit 1
fi

BOB_TUNNEL_IP="$(printf '%s' "$ALICE_COMPACT" | peer_tunnel_ip_from_status)"
ALICE_TUNNEL_IP="$(printf '%s' "$BOB_COMPACT" | peer_tunnel_ip_from_status)"

if [[ -z "$ALICE_TUNNEL_IP" || -z "$BOB_TUNNEL_IP" ]]; then
  echo "nat docker e2e failed: unable to resolve peer tunnel IPs from status output" >&2
  exit 1
fi

sleep 5

ping_until_success node-a "$BOB_TUNNEL_IP" /tmp/nvpn-nat-ping-a.log
ping_until_success node-b "$ALICE_TUNNEL_IP" /tmp/nvpn-nat-ping-b.log

ALICE_STATUS="$("${COMPOSE[@]}" exec -T node-a nvpn status --json --discover-secs 0 | tr -d '\r')"
BOB_STATUS="$("${COMPOSE[@]}" exec -T node-b nvpn status --json --discover-secs 0 | tr -d '\r')"
ALICE_COMPACT="$(printf '%s' "$ALICE_STATUS" | compact_json)"
BOB_COMPACT="$(printf '%s' "$BOB_STATUS" | compact_json)"

grep -q '"reachable":true' <<<"$ALICE_COMPACT"
grep -q '"reachable":true' <<<"$BOB_COMPACT"

echo "--- Ping A -> B ---"
cat /tmp/nvpn-nat-ping-a.log
echo "--- Ping B -> A ---"
cat /tmp/nvpn-nat-ping-b.log

echo "nat docker e2e passed: daemon-mode Nostr signaling, public endpoint discovery, NAT punching from a private node to a public peer, boringtun tunnel handshake, and ping succeeded"
