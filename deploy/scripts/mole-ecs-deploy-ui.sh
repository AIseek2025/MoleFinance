#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
FRONTEND_DIR="$ROOT/frontend"
SSH_HOST="${MOLE_SSH_HOST:-admin@8.218.209.218}"
REMOTE_BASE="${MOLE_REMOTE_BASE:-/var/www/molefinance}"
STAMP="$(date +%Y%m%d-%H%M%S)"
REMOTE_RELEASE="$REMOTE_BASE/releases/$STAMP"

cd "$ROOT"

echo "=== Mole ECS UI Deploy ==="
echo "SSH:     $SSH_HOST"
echo "Release: $REMOTE_RELEASE"
echo

if [[ ! -f "$FRONTEND_DIR/.env.production.local" ]]; then
  echo "ERROR: missing frontend/.env.production.local"
  exit 1
fi

echo "[1/4] Build frontend"
(
  cd "$FRONTEND_DIR"
  if [[ ! -d node_modules ]]; then
    npm ci
  fi
  npm run build
)

echo "[2/4] Prepare remote release"
ssh -o StrictHostKeyChecking=no "$SSH_HOST" "
  sudo mkdir -p '$REMOTE_BASE/releases' '$REMOTE_BASE/certbot'
  sudo chown -R admin:admin '$REMOTE_BASE'
  mkdir -p '$REMOTE_RELEASE'
"

echo "[3/4] Upload dist"
rsync -az --delete \
  -e "ssh -o StrictHostKeyChecking=no" \
  "$FRONTEND_DIR/dist/" "$SSH_HOST:$REMOTE_RELEASE/"

echo "[4/4] Switch current symlink"
ssh -o StrictHostKeyChecking=no "$SSH_HOST" "
  PREV=\$(readlink -f '$REMOTE_BASE/current' 2>/dev/null || true)
  ln -sfn '$REMOTE_RELEASE' '$REMOTE_BASE/current'
  echo PREVIOUS=\${PREV:-<none>}
  echo CURRENT=\$(readlink -f '$REMOTE_BASE/current')
"

echo "=== Deploy complete ==="
