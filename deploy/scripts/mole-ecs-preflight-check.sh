#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SSH_HOST="${MOLE_SSH_HOST:-admin@8.218.209.218}"
REMOTE_BASE="${MOLE_REMOTE_BASE:-/var/www/molefinance}"
EXPECTED_IP="${MOLE_EXPECTED_IP:-8.218.209.218}"
FRONTEND_DIR="$ROOT/frontend"

echo "=== Mole ECS Preflight ==="
echo "ROOT: $ROOT"
echo "SSH:  $SSH_HOST"
echo

echo "[1/5] DNS"
for host in molefinance.net www.molefinance.net; do
  actual="$(dig +short "$host" | tail -n 1 || true)"
  printf "  %-22s -> %s\n" "$host" "${actual:-<empty>}"
  if [[ -n "$EXPECTED_IP" && "${actual:-}" != "$EXPECTED_IP" ]]; then
    echo "  WARN: expected $EXPECTED_IP"
  fi
done

echo "[2/5] SSH + sudo"
ssh -o StrictHostKeyChecking=no "$SSH_HOST" \
  'echo "  SSH_OK $(hostname) $(whoami)"; sudo -n true && echo "  SUDO_OK"'

echo "[3/5] Remote runtime"
ssh -o StrictHostKeyChecking=no "$SSH_HOST" "
  printf '  nginx:   '; nginx -v 2>&1 || true
  printf '  certbot: '; certbot --version 2>&1 || true
  printf '  base:    '; test -d '$REMOTE_BASE' && echo OK || echo MISSING
  printf '  current: '; readlink -f '$REMOTE_BASE/current' 2>/dev/null || echo '<none>'
"

echo "[4/5] Frontend env"
if [[ -f "$FRONTEND_DIR/.env.production.local" ]]; then
  echo "  frontend/.env.production.local: OK"
else
  echo "  frontend/.env.production.local: MISSING"
fi

echo "[5/5] Local build prereqs"
printf "  node: "; node -v
printf "  npm:  "; npm -v
if [[ -d "$FRONTEND_DIR/dist" ]]; then
  echo "  frontend/dist: PRESENT"
else
  echo "  frontend/dist: MISSING (script will build)"
fi

echo "=== Preflight complete ==="
