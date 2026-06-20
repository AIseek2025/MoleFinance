#!/bin/bash
# Wrapper that launchd (com.moleoption.keeper) execs to keep the devnet keeper
# loop running. Kept as a tiny shell shim so the plist needs no inline env and
# so you can also run it by hand:  frontend/scripts/keeper-daemon.sh
set -euo pipefail

# launchd hands processes a minimal PATH; pin the dirs the keeper needs.
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"

# Public devnet RPC: only light calls (getLatestBlockhash / sendTransaction /
# getAccountInfo), which it serves fine. Override by exporting SOLANA_RPC_URL.
export SOLANA_RPC_URL="${SOLANA_RPC_URL:-https://api.devnet.solana.com}"
export KEEPER_INTERVAL_MS="${KEEPER_INTERVAL_MS:-6000}"

cd "$(dirname "$0")/.."
exec node scripts/keeper-devnet.mjs run
