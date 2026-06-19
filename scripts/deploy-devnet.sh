#!/usr/bin/env bash
# MoleOption - devnet deploy helper (wave 30).
#
# One command, idempotent, NO anchor CLI required (uses the already-
# installed `cargo build-sbf` + `solana program deploy`). Your wallet
# secret NEVER leaves this machine: it is read from a local keypair file
# ($SOLANA_KEYPAIR or ~/.config/solana/id.json), never printed, never
# committed.
#
# Usage:
#   scripts/deploy-devnet.sh            # full flow: prep -> build -> fund -> deploy
#   scripts/deploy-devnet.sh prep       # wallet + program keypair + declare_id backfill
#   scripts/deploy-devnet.sh build      # cargo build-sbf only
#   scripts/deploy-devnet.sh fund       # airdrop devnet SOL to the wallet
#   scripts/deploy-devnet.sh deploy     # solana program deploy (requires built .so + funds)
#   scripts/deploy-devnet.sh reclaim    # close orphaned buffer accounts, recover their SOL
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

PROGRAM_DIR="programs/mole-option"
LIB_RS="$PROGRAM_DIR/src/lib.rs"
WALLET="${SOLANA_KEYPAIR:-$HOME/.config/solana/id.json}"
PROGRAM_KEYPAIR="$PROGRAM_DIR/target/deploy/mole_option-keypair.json"
SO_PATH="$PROGRAM_DIR/target/deploy/mole_option.so"
WALLET_PUBKEY=""
PROGRAM_ID=""
# The public devnet RPC (api.devnet.solana.com) rate-limits (HTTP 429)
# hard and CANNOT reliably deploy a ~680 KB program (hundreds of chunk
# txs). Set SOLANA_RPC_URL to a dedicated devnet endpoint (free tiers:
# Helius / QuickNode / Alchemy) for a clean one-shot deploy.
CLUSTER_URL="${SOLANA_RPC_URL:-https://api.devnet.solana.com}"

log() { printf '\033[36m[deploy]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[deploy]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31m[deploy:error]\033[0m %s\n' "$*" >&2; exit 1; }

ensure_wallet() {
  if [[ ! -f "$WALLET" ]]; then
    log "creating a fresh DEVNET wallet at $WALLET (secret stays local, not printed)"
    mkdir -p "$(dirname "$WALLET")"
    solana-keygen new --silent --no-bip39-passphrase -o "$WALLET" >/dev/null
  fi
  solana config set --url "$CLUSTER_URL" --keypair "$WALLET" >/dev/null
  WALLET_PUBKEY="$(solana-keygen pubkey "$WALLET")"
  log "wallet: ${WALLET_PUBKEY}"
}

ensure_program_keypair() {
  if [[ ! -f "$PROGRAM_KEYPAIR" ]]; then
    log "generating program keypair at $PROGRAM_KEYPAIR"
    mkdir -p "$(dirname "$PROGRAM_KEYPAIR")"
    solana-keygen new --silent --no-bip39-passphrase -o "$PROGRAM_KEYPAIR" >/dev/null
  fi
  PROGRAM_ID="$(solana-keygen pubkey "$PROGRAM_KEYPAIR")"
  log "program id: ${PROGRAM_ID}"
}

backfill_program_id() {
  # declare_id! in lib.rs MUST equal the deploy address.
  sed -i '' "s/declare_id!(\"[^\"]*\")/declare_id!(\"$PROGRAM_ID\")/" "$LIB_RS"
  # Anchor.toml (informational for our CLI-deploy path; keep consistent).
  sed -i '' 's/\[programs.localnet\]/[programs.devnet]/' Anchor.toml || true
  sed -i '' "s#mole_option = \"[^\"]*\"#mole_option = \"$PROGRAM_ID\"#" Anchor.toml || true
  sed -i '' 's/cluster = "Localnet"/cluster = "Devnet"/' Anchor.toml || true
  log "backfilled declare_id! + Anchor.toml with ${PROGRAM_ID}"
}

cmd_prep() {
  ensure_wallet
  ensure_program_keypair
  backfill_program_id
}

cmd_build() {
  ensure_program_keypair
  log "building SBF program (cargo build-sbf) - first build can take several minutes..."
  # The program's [dev-dependencies] (solana-program-test, used only by
  # the gated CI test matrix) drag in a YANKED solana_rbpf 0.8.1 that
  # breaks dependency resolution. They are NOT part of the deployable
  # cdylib, so strip them for the SBF build only and restore afterward
  # (trap covers failure / interrupt). The committed manifest - and CI -
  # are untouched.
  local manifest="$PROGRAM_DIR/Cargo.toml"
  local backup; backup="$(mktemp)"
  cp "$manifest" "$backup"
  restore_manifest() { [[ -f "$backup" ]] && cp "$backup" "$manifest" && rm -f "$backup"; trap - EXIT INT TERM; }
  # Cover Ctrl-C / kill too, so an interrupted build never leaves the
  # manifest with its dev-dependencies stripped.
  trap restore_manifest EXIT INT TERM
  # [dev-dependencies] is the last section of the manifest.
  sed -i '' '/^\[dev-dependencies\]/,$d' "$manifest"

  # Use the pre-mounted platform-tools (installed by
  # scripts/install-platform-tools.sh) and do NOT let cargo-build-sbf
  # manage the toolchain (its downloader has no resume and corrupts the
  # tree on a flaky link). --skip-tools-install: never (re)download.
  local pt="$HOME/.cache/solana/v1.52/platform-tools"
  if [[ -d "$pt/rust/bin" ]]; then
    if [[ "$(uname -s)" == "Darwin" ]]; then
      find "$pt" -type f -exec xattr -d com.apple.quarantine {} \; 2>/dev/null || true
    fi
    (
      cd "$PROGRAM_DIR"
      PATH="$pt/rust/bin:$pt/llvm/bin:$PATH" RUSTC="$pt/rust/bin/rustc" \
        cargo-build-sbf --skip-tools-install --no-rustup-override --sbf-out-dir target/deploy
    )
  else
    warn "platform-tools not found at $pt - run scripts/install-platform-tools.sh first"
    ( cd "$PROGRAM_DIR" && cargo build-sbf --sbf-out-dir target/deploy )
  fi
  restore_manifest
  [[ -f "$SO_PATH" ]] || die "build did not produce $SO_PATH"
  log "built: $SO_PATH ($(du -h "$SO_PATH" | cut -f1))"
}

cmd_fund() {
  ensure_wallet
  local bal
  bal="$(solana balance "$WALLET_PUBKEY" 2>/dev/null | awk '{print $1}')" || bal=0
  log "current balance: ${bal:-0} SOL"
  if awk "BEGIN{exit !(${bal:-0} >= 6)}"; then
    log "wallet already funded (>= 6 SOL)"; return 0
  fi
  for i in 1 2 3; do
    log "airdrop attempt $i/3 (2 SOL)..."
    if solana airdrop 2 "$WALLET_PUBKEY" 2>/dev/null; then
      log "airdrop ok"
    fi
    sleep 5
  done
  bal="$(solana balance "$WALLET_PUBKEY" 2>/dev/null | awk '{print $1}')" || bal=0
  if awk "BEGIN{exit !(${bal:-0} >= 6)}"; then return 0; fi
  warn "wallet has ${bal:-0} SOL; deploying a ~680KB program needs ~5-6 SOL."
  warn "Top up then re-run 'deploy':"
  warn "  https://faucet.solana.com  ->  paste address: $WALLET_PUBKEY"
  return 1
}

cmd_deploy() {
  ensure_wallet
  ensure_program_keypair
  [[ -f "$SO_PATH" ]] || die "no built program at $SO_PATH - run 'build' first"
  log "deploying to ${CLUSTER_URL}..."
  # Public devnet RPC drops a lot of TPU-forwarded writes, which shows
  # up as "Max retries exceeded" partway through the buffer upload.
  #   --use-rpc                : send writes via the RPC node, not TPU.
  #   --with-compute-unit-price: small priority fee so writes land.
  #   --max-sign-attempts      : re-sign/re-send unconfirmed writes more.
  solana program deploy "$SO_PATH" \
    --program-id "$PROGRAM_KEYPAIR" \
    --keypair "$WALLET" \
    --url "$CLUSTER_URL" \
    --use-rpc \
    --with-compute-unit-price 50000 \
    --max-sign-attempts 200
  # Write the program id into the frontend env so the UI talks to it.
  local env_file="frontend/.env.local"
  {
    echo "VITE_RPC_URL=$CLUSTER_URL"
    echo "VITE_MOLE_PROGRAM_ID=$PROGRAM_ID"
  } > "$env_file"
  log "deployed OK  program id: ${PROGRAM_ID}"
  log "frontend env written: $env_file"
  log "explorer: https://explorer.solana.com/address/${PROGRAM_ID}?cluster=devnet"
}

cmd_reclaim() {
  ensure_wallet
  # A deploy that dies mid buffer-upload (e.g. RPC 429 / blockhash expiry)
  # leaves an orphaned buffer account holding the rent + uploaded chunks'
  # SOL (for a ~680KB program that is ~5 SOL!). Closing all buffers owned
  # by this wallet recovers that SOL. Needs a working RPC - set
  # SOLANA_RPC_URL if the public endpoint is rate-limiting you.
  log "closing orphaned buffer accounts owned by ${WALLET_PUBKEY}..."
  solana program close --buffers \
    --keypair "$WALLET" \
    --url "$CLUSTER_URL" \
    --bypass-warning
  log "balance after reclaim: $(solana balance "$WALLET_PUBKEY" --url "$CLUSTER_URL" 2>&1)"
}

cmd_show() {
  ensure_wallet
  ensure_program_keypair
  log "balance: $(solana balance "$WALLET_PUBKEY" --url "$CLUSTER_URL" 2>&1)"
  log "program account ${PROGRAM_ID}:"
  solana program show "$PROGRAM_ID" --url "$CLUSTER_URL" 2>&1 \
    || warn "program account not found / not a deployed program"
}

cmd_fresh() {
  # Rotate to a brand-new program id. This sidesteps BOTH failure modes
  # of a wedged deploy: a half-initialised program account from a prior
  # attempt, and any stale/incomplete buffer. The old program keypair is
  # archived (not deleted). Rebuilds (declare_id changes) then deploys.
  log "rotating to a FRESH program id (archiving the old keypair)..."
  if [[ -f "$PROGRAM_KEYPAIR" ]]; then
    mv "$PROGRAM_KEYPAIR" "${PROGRAM_KEYPAIR}.bak.$(date +%s)"
  fi
  ensure_program_keypair
  backfill_program_id
  cmd_build
  cmd_deploy
}

case "${1:-all}" in
  prep)    cmd_prep ;;
  build)   cmd_build ;;
  fund)    cmd_fund ;;
  deploy)  cmd_deploy ;;
  reclaim) cmd_reclaim ;;
  show)    cmd_show ;;
  fresh)   cmd_fresh ;;
  all)     cmd_prep; cmd_build; cmd_fund && cmd_deploy || warn "fund the wallet, then run: scripts/deploy-devnet.sh deploy" ;;
  *)       die "unknown command '$1' (use: prep|build|fund|deploy|reclaim|show|fresh|all)" ;;
esac
