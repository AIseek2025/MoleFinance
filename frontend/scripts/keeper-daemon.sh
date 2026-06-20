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
export KEEPER_INTERVAL_MS="${KEEPER_INTERVAL_MS:-8000}"

# Which catalog bases to keep live on devnet. Must match what was provisioned by
# `keeper-devnet.mjs setup`. Override by exporting MARKET_BASES before launch.
export MARKET_BASES="${MARKET_BASES:-SOL,BTC,ETH,SP500,GOLD,EURUSD}"

cd "$(dirname "$0")/.."
exec node scripts/keeper-devnet.mjs run
