#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
COMPOSE=(docker compose -f "$ROOT_DIR/docker-compose.relay-fallback.e2e.yml")

RELAY_URL="ws://10.203.1.2:8080"
CONFIG_PATH="/root/.config/nvpn/config.toml"
GOOD_RELAY_IP="10.203.1.3"
ASYM_RELAY_IP="10.203.1.4"
ALICE_IP="10.203.1.10"
BOB_IP="10.203.1.11"

cleanup() {
  "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
}

dump_debug() {
  set +e
  echo "relay fallback docker e2e failed, collecting debug output..."
  "${COMPOSE[@]}" ps || true
  for service in nostr-relay relay-good relay-asym; do
    echo "--- compose logs: ${service} ---"
    "${COMPOSE[@]}" logs --no-color --tail 200 "$service" || true
  done
  echo "--- nostr-relay /tmp/nostr-relay.log ---"
  "${COMPOSE[@]}" exec -T nostr-relay sh -lc "cat /tmp/nostr-relay.log 2>/dev/null || true" || true
  for service in relay-good relay-asym; do
    echo "--- ${service} /tmp/nvpn-udp-relay.log ---"
    "${COMPOSE[@]}" exec -T "$service" sh -lc "cat /tmp/nvpn-udp-relay.log 2>/dev/null || true" || true
    echo "--- ${service} relay.operator.json ---"
    "${COMPOSE[@]}" exec -T "$service" sh -lc "cat /root/.config/nvpn/relay.operator.json 2>/dev/null || true" || true
  done
  for node in node-a node-b; do
    echo "--- ${node} status ---"
    "${COMPOSE[@]}" exec -T "$node" nvpn status --json --discover-secs 0 || true
    echo "--- ${node} daemon.state.json ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "cat /root/.config/nvpn/daemon.state.json 2>/dev/null || true" || true
    echo "--- ${node} daemon.log ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "tail -n 200 /root/.config/nvpn/daemon.log 2>/dev/null || true" || true
    echo "--- ${node} wireguard socket ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "ls -l /var/run/wireguard/utun100.sock 2>/dev/null || true" || true
    echo "--- ${node} routes ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "ip route || true" || true
    echo "--- ${node} utun100 ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "ip addr show utun100 || true" || true
    echo "--- ${node} iptables ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "iptables -S || true" || true
    echo "--- ${node} processes ---"
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

peer_runtime_endpoint_from_status() {
  grep -o '"runtime_endpoint":"[^"]*"' | tail -n1 | cut -d '"' -f4 || true
}

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

  echo "relay fallback docker e2e failed: service '$service' did not reach running state" >&2
  exit 1
}

ping_until_success() {
  local node="$1"
  local target="$2"
  local log_path="$3"
  for _ in $(seq 1 6); do
    if "${COMPOSE[@]}" exec -T "$node" ping -c 3 -W 2 "$target" >"$log_path"; then
      return 0
    fi
    sleep 2
  done

  return 1
}

cleanup

"${COMPOSE[@]}" build >/dev/null
"${COMPOSE[@]}" up -d nostr-relay relay-asym relay-good node-a node-b >/dev/null

for service in nostr-relay relay-asym relay-good node-a node-b; do
  wait_for_service "$service"
done

sleep 6

for node in node-a node-b; do
  "${COMPOSE[@]}" exec -T "$node" nvpn init --force >/dev/null
  "${COMPOSE[@]}" exec -T "$node" sh -lc \
    "perl -0pi -e 's/\\[nat\\]\\nenabled = true/\\[nat\\]\\nenabled = false/' '$CONFIG_PATH'"
done

ALICE_NPUB="$("${COMPOSE[@]}" exec -T node-a sh -lc \
  "grep -m1 '^public_key' '$CONFIG_PATH' | cut -d '\"' -f 2" | tr -d '\r')"
BOB_NPUB="$("${COMPOSE[@]}" exec -T node-b sh -lc \
  "grep -m1 '^public_key' '$CONFIG_PATH' | cut -d '\"' -f 2" | tr -d '\r')"

if [[ -z "$ALICE_NPUB" || -z "$BOB_NPUB" ]]; then
  echo "relay fallback docker e2e failed: unable to resolve node npubs" >&2
  exit 1
fi

"${COMPOSE[@]}" exec -T node-a nvpn set --participant "$BOB_NPUB" --relay "$RELAY_URL" >/dev/null
"${COMPOSE[@]}" exec -T node-b nvpn set --participant "$ALICE_NPUB" --relay "$RELAY_URL" >/dev/null

"${COMPOSE[@]}" exec -T node-a sh -lc \
  "iptables -I OUTPUT 1 -p udp -d $BOB_IP --dport 51820 -j REJECT"
"${COMPOSE[@]}" exec -T node-b sh -lc \
  "iptables -I OUTPUT 1 -p udp -d $ALICE_IP --dport 51820 -j REJECT"
"${COMPOSE[@]}" exec -T node-b sh -lc \
  "iptables -I OUTPUT 1 -p udp -d $ASYM_RELAY_IP -j REJECT"

"${COMPOSE[@]}" exec -T node-a nvpn start --daemon --connect --announce-interval-secs 5 >/dev/null
"${COMPOSE[@]}" exec -T node-b nvpn start --daemon --connect --announce-interval-secs 5 >/dev/null

ALICE_STATUS=""
BOB_STATUS=""
ALICE_COMPACT=""
BOB_COMPACT=""
ALICE_ANNOUNCED_ENDPOINT=""
BOB_ANNOUNCED_ENDPOINT=""
ALICE_RUNTIME_ENDPOINT=""
BOB_RUNTIME_ENDPOINT=""
ALICE_TUNNEL_IP=""
BOB_TUNNEL_IP=""
for _ in $(seq 1 120); do
  ALICE_STATUS="$("${COMPOSE[@]}" exec -T node-a nvpn status --json --discover-secs 0 | tr -d '\r')"
  BOB_STATUS="$("${COMPOSE[@]}" exec -T node-b nvpn status --json --discover-secs 0 | tr -d '\r')"
  ALICE_COMPACT="$(printf '%s' "$ALICE_STATUS" | compact_json)"
  BOB_COMPACT="$(printf '%s' "$BOB_STATUS" | compact_json)"
  ALICE_ANNOUNCED_ENDPOINT="$(printf '%s' "$ALICE_COMPACT" | peer_announced_endpoint_from_status)"
  BOB_ANNOUNCED_ENDPOINT="$(printf '%s' "$BOB_COMPACT" | peer_announced_endpoint_from_status)"
  ALICE_RUNTIME_ENDPOINT="$(printf '%s' "$ALICE_COMPACT" | peer_runtime_endpoint_from_status)"
  BOB_RUNTIME_ENDPOINT="$(printf '%s' "$BOB_COMPACT" | peer_runtime_endpoint_from_status)"
  BOB_TUNNEL_IP="$(printf '%s' "$ALICE_COMPACT" | peer_tunnel_ip_from_status)"
  ALICE_TUNNEL_IP="$(printf '%s' "$BOB_COMPACT" | peer_tunnel_ip_from_status)"

  if grep -q '"status_source":"daemon"' <<<"$ALICE_COMPACT" \
    && grep -q '"status_source":"daemon"' <<<"$BOB_COMPACT" \
    && grep -q '"running":true' <<<"$ALICE_COMPACT" \
    && grep -q '"running":true' <<<"$BOB_COMPACT" \
    && [[ "$ALICE_ANNOUNCED_ENDPOINT" == "$BOB_IP:51820" ]] \
    && [[ "$BOB_ANNOUNCED_ENDPOINT" == "$ALICE_IP:51820" ]] \
    && [[ "$ALICE_RUNTIME_ENDPOINT" == "$GOOD_RELAY_IP:"* ]] \
    && [[ "$BOB_RUNTIME_ENDPOINT" == "$GOOD_RELAY_IP:"* ]] \
    && [[ "$ALICE_RUNTIME_ENDPOINT" != "$ALICE_ANNOUNCED_ENDPOINT" ]] \
    && [[ "$BOB_RUNTIME_ENDPOINT" != "$BOB_ANNOUNCED_ENDPOINT" ]] \
    && [[ -n "$ALICE_TUNNEL_IP" ]] \
    && [[ -n "$BOB_TUNNEL_IP" ]]; then
    break
  fi
  sleep 1
done

printf 'ALICE STATUS\n%s\n' "$ALICE_STATUS"
printf 'BOB STATUS\n%s\n' "$BOB_STATUS"

grep -q '"status_source":"daemon"' <<<"$ALICE_COMPACT"
grep -q '"status_source":"daemon"' <<<"$BOB_COMPACT"
grep -q '"running":true' <<<"$ALICE_COMPACT"
grep -q '"running":true' <<<"$BOB_COMPACT"

if [[ "$ALICE_ANNOUNCED_ENDPOINT" != "$BOB_IP:51820" ]]; then
  echo "relay fallback docker e2e failed: alice did not keep bob's direct announced endpoint ('$ALICE_ANNOUNCED_ENDPOINT')" >&2
  exit 1
fi
if [[ "$BOB_ANNOUNCED_ENDPOINT" != "$ALICE_IP:51820" ]]; then
  echo "relay fallback docker e2e failed: bob did not keep alice's direct announced endpoint ('$BOB_ANNOUNCED_ENDPOINT')" >&2
  exit 1
fi
if [[ "$ALICE_RUNTIME_ENDPOINT" != "$GOOD_RELAY_IP:"* ]]; then
  echo "relay fallback docker e2e failed: alice runtime endpoint did not switch to the live relay ('$ALICE_RUNTIME_ENDPOINT')" >&2
  exit 1
fi
if [[ "$BOB_RUNTIME_ENDPOINT" != "$GOOD_RELAY_IP:"* ]]; then
  echo "relay fallback docker e2e failed: bob runtime endpoint did not switch to the live relay ('$BOB_RUNTIME_ENDPOINT')" >&2
  exit 1
fi
if [[ -z "$ALICE_TUNNEL_IP" || -z "$BOB_TUNNEL_IP" ]]; then
  echo "relay fallback docker e2e failed: unable to resolve peer tunnel IPs from status output" >&2
  exit 1
fi

sleep 3

ping_until_success node-a "$BOB_TUNNEL_IP" /tmp/nvpn-relay-ping-a.log
ping_until_success node-b "$ALICE_TUNNEL_IP" /tmp/nvpn-relay-ping-b.log

ALICE_STATUS="$("${COMPOSE[@]}" exec -T node-a nvpn status --json --discover-secs 0 | tr -d '\r')"
BOB_STATUS="$("${COMPOSE[@]}" exec -T node-b nvpn status --json --discover-secs 0 | tr -d '\r')"
ALICE_COMPACT="$(printf '%s' "$ALICE_STATUS" | compact_json)"
BOB_COMPACT="$(printf '%s' "$BOB_STATUS" | compact_json)"

grep -q '"reachable":true' <<<"$ALICE_COMPACT"
grep -q '"reachable":true' <<<"$BOB_COMPACT"

ASYM_SERVICE_STATE="$("${COMPOSE[@]}" exec -T relay-asym sh -lc "cat /root/.config/nvpn/relay.operator.json" | tr -d '\r')"
GOOD_SERVICE_STATE="$("${COMPOSE[@]}" exec -T relay-good sh -lc "cat /root/.config/nvpn/relay.operator.json" | tr -d '\r')"
ALICE_DAEMON_LOG="$("${COMPOSE[@]}" exec -T node-a sh -lc "cat /root/.config/nvpn/daemon.log 2>/dev/null || true" | tr -d '\r')"
BOB_DAEMON_LOG="$("${COMPOSE[@]}" exec -T node-b sh -lc "cat /root/.config/nvpn/daemon.log 2>/dev/null || true" | tr -d '\r')"

grep -Eq '"total_sessions_served"[[:space:]]*:[[:space:]]*[1-9]' <<<"$ASYM_SERVICE_STATE"
grep -Eq '"total_sessions_served"[[:space:]]*:[[:space:]]*[1-9]' <<<"$GOOD_SERVICE_STATE"
grep -q 'relay: proactive probe verified' <<<"$ALICE_DAEMON_LOG$BOB_DAEMON_LOG"

echo "--- Ping A -> B ---"
cat /tmp/nvpn-relay-ping-a.log
echo "--- Ping B -> A ---"
cat /tmp/nvpn-relay-ping-b.log

echo "relay fallback docker e2e passed: direct peer UDP was blocked, proactive relay verification ran, both relay operators were exercised, both peers converged on the mutually reachable relay ingress, and tunnel ping succeeded through the relay operator"
