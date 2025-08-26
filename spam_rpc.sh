#!/usr/bin/env bash
set -euo pipefail

CMD='RUST_LOG=debug target/release/anvil-polkadot'
URL='http://127.0.0.1:8545'
PAYLOAD='{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}'
INTERVAL=0.03
# --------------------

bash -c "$CMD" & PID=$!

sleep 2

while true; do
  curl -sS -H "Content-Type: application/json" -d "$PAYLOAD" "$URL" >/dev/null 2>&1 &
  sleep "$INTERVAL"
done