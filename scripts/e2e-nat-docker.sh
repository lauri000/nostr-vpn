#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
COMPOSE=(docker compose -f "$ROOT_DIR/docker-compose.nat-e2e.yml")

NETWORK_ID="docker-vpn-nat"
RELAY_URL="ws://10.254.241.2:8080"
REFLECTOR_ADDR="10.254.241.3:3478"

ALICE_WG_PRIVATE="9eUzwIuYiF1Au6fBSwSnMHuWp90mqFQZrsC3YH7qzb8="
ALICE_WG_PUBLIC="8VBKKEKhzF7lPlukFYvpMsZX42RcgClBcwI1FpFTIRE="
BOB_WG_PRIVATE="3DnM5OoFTb2DgGQ/BPAM0W8+xKUExtxA1l5jXloihO0="
BOB_WG_PUBLIC="czraiWsRqvnjWLMoww0riN8uZa6By7EJl6swa5mY5To="

cleanup() {
  "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
}

dump_debug() {
  set +e
  echo "nat e2e failed, collecting debug logs..."
  "${COMPOSE[@]}" ps || true
  for service in relay reflector nat-a nat-b node-a node-b; do
    echo "--- logs: $service ---"
    "${COMPOSE[@]}" logs --no-color --tail 120 "$service" || true
  done
  for node in node-a node-b; do
    echo "--- $node /tmp/tunnel.log ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "cat /tmp/tunnel.log 2>/dev/null || true" || true
    echo "--- $node routes ---"
    "${COMPOSE[@]}" exec -T "$node" sh -lc "ip route || true" || true
    "${COMPOSE[@]}" exec -T "$node" sh -lc "ip addr show utun100 || true" || true
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

cleanup

"${COMPOSE[@]}" build >/dev/null
"${COMPOSE[@]}" up -d relay reflector nat-a nat-b node-a node-b >/dev/null
sleep 3

"${COMPOSE[@]}" exec -T node-a sh -lc \
  "ip route del default >/dev/null 2>&1 || true; ip route add default via 198.19.241.2 dev eth0"
"${COMPOSE[@]}" exec -T node-b sh -lc \
  "ip route del default >/dev/null 2>&1 || true; ip route add default via 198.19.242.2 dev eth0"

ALICE_NPUB="$("${COMPOSE[@]}" exec -T node-a sh -lc \
  "nvpn init --force >/dev/null && grep -m1 '^public_key' /root/.config/nvpn/config.toml | cut -d '\"' -f 2")"
BOB_NPUB="$("${COMPOSE[@]}" exec -T node-b sh -lc \
  "nvpn init --force >/dev/null && grep -m1 '^public_key' /root/.config/nvpn/config.toml | cut -d '\"' -f 2")"

if [[ -z "$ALICE_NPUB" || -z "$BOB_NPUB" ]]; then
  echo "nat e2e failed: unable to resolve node npubs from config" >&2
  exit 1
fi

ALICE_ENDPOINT="$("${COMPOSE[@]}" exec -T node-a nvpn nat-discover --reflector "$REFLECTOR_ADDR" --listen-port 51820 | tr -d '\r' | tail -n1)"
BOB_ENDPOINT="$("${COMPOSE[@]}" exec -T node-b nvpn nat-discover --reflector "$REFLECTOR_ADDR" --listen-port 51820 | tr -d '\r' | tail -n1)"

if [[ -z "$ALICE_ENDPOINT" || -z "$BOB_ENDPOINT" ]]; then
  echo "nat e2e failed: endpoint discovery returned empty result" >&2
  exit 1
fi

echo "alice endpoint: $ALICE_ENDPOINT"
echo "bob endpoint:   $BOB_ENDPOINT"

"${COMPOSE[@]}" exec -T node-a sh -lc \
  "nvpn listen --network-id '$NETWORK_ID' --relay '$RELAY_URL' --participant '$ALICE_NPUB' --participant '$BOB_NPUB' --limit 1 > /tmp/listen.log 2>&1 &"
"${COMPOSE[@]}" exec -T node-b sh -lc \
  "nvpn listen --network-id '$NETWORK_ID' --relay '$RELAY_URL' --participant '$ALICE_NPUB' --participant '$BOB_NPUB' --limit 1 > /tmp/listen.log 2>&1 &"

sleep 2

"${COMPOSE[@]}" exec -T node-a nvpn announce \
  --network-id "$NETWORK_ID" \
  --relay "$RELAY_URL" \
  --participant "$ALICE_NPUB" \
  --participant "$BOB_NPUB" \
  --node-id alice-node \
  --endpoint "$ALICE_ENDPOINT" \
  --tunnel-ip 10.44.0.1/32 \
  --public-key "$ALICE_WG_PUBLIC" >/dev/null

"${COMPOSE[@]}" exec -T node-b nvpn announce \
  --network-id "$NETWORK_ID" \
  --relay "$RELAY_URL" \
  --participant "$BOB_NPUB" \
  --participant "$ALICE_NPUB" \
  --node-id bob-node \
  --endpoint "$BOB_ENDPOINT" \
  --tunnel-ip 10.44.0.2/32 \
  --public-key "$BOB_WG_PUBLIC" >/dev/null

for _ in $(seq 1 25); do
  ALICE_LISTEN_LOGS="$("${COMPOSE[@]}" exec -T node-a sh -lc 'cat /tmp/listen.log 2>/dev/null || true')"
  BOB_LISTEN_LOGS="$("${COMPOSE[@]}" exec -T node-b sh -lc 'cat /tmp/listen.log 2>/dev/null || true')"

  if grep -Eq '"node_id"\s*:\s*"bob-node"' <<<"$ALICE_LISTEN_LOGS" \
    && grep -Eq '"node_id"\s*:\s*"alice-node"' <<<"$BOB_LISTEN_LOGS"; then
    break
  fi
  sleep 1
done

ALICE_LISTEN_LOGS="$("${COMPOSE[@]}" exec -T node-a sh -lc 'cat /tmp/listen.log 2>/dev/null || true')"
BOB_LISTEN_LOGS="$("${COMPOSE[@]}" exec -T node-b sh -lc 'cat /tmp/listen.log 2>/dev/null || true')"

if ! grep -Eq '"node_id"\s*:\s*"bob-node"' <<<"$ALICE_LISTEN_LOGS"; then
  echo "nat e2e failed: alice did not receive bob announcement" >&2
  echo "$ALICE_LISTEN_LOGS"
  exit 1
fi

if ! grep -Eq '"node_id"\s*:\s*"alice-node"' <<<"$BOB_LISTEN_LOGS"; then
  echo "nat e2e failed: bob did not receive alice announcement" >&2
  echo "$BOB_LISTEN_LOGS"
  exit 1
fi

"${COMPOSE[@]}" exec -d node-a sh -lc \
  "nvpn tunnel-up \
     --iface utun100 \
     --private-key '$ALICE_WG_PRIVATE' \
     --listen-port 51820 \
     --address 10.44.0.1/32 \
     --peer-public-key '$BOB_WG_PUBLIC' \
     --peer-endpoint '$BOB_ENDPOINT' \
     --peer-allowed-ip 10.44.0.2/32 \
     --keepalive-secs 1 \
     --hole-punch-attempts 80 \
     --hole-punch-interval-ms 120 \
     --hole-punch-recv-timeout-ms 120 > /tmp/tunnel.log 2>&1"

"${COMPOSE[@]}" exec -d node-b sh -lc \
  "nvpn tunnel-up \
     --iface utun100 \
     --private-key '$BOB_WG_PRIVATE' \
     --listen-port 51820 \
     --address 10.44.0.2/32 \
     --peer-public-key '$ALICE_WG_PUBLIC' \
     --peer-endpoint '$ALICE_ENDPOINT' \
     --peer-allowed-ip 10.44.0.1/32 \
     --keepalive-secs 1 \
     --hole-punch-attempts 80 \
     --hole-punch-interval-ms 120 \
     --hole-punch-recv-timeout-ms 120 > /tmp/tunnel.log 2>&1"

sleep 14

"${COMPOSE[@]}" exec -T node-a ping -c 3 -W 2 10.44.0.2 >/tmp/ping-a.log
"${COMPOSE[@]}" exec -T node-b ping -c 3 -W 2 10.44.0.1 >/tmp/ping-b.log

echo "--- Alice listen log ---"
echo "$ALICE_LISTEN_LOGS"
echo "--- Bob listen log ---"
echo "$BOB_LISTEN_LOGS"
echo "--- Ping A -> B ---"
cat /tmp/ping-a.log
echo "--- Ping B -> A ---"
cat /tmp/ping-b.log

echo "nat docker e2e passed: Nostr signaling + UDP punch + boringtun tunnel ping succeeded"
