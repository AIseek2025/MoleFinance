#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SSH_HOST="${MOLE_SSH_HOST:-admin@8.218.209.218}"
REMOTE_BASE="${MOLE_REMOTE_BASE:-/var/www/molefinance}"

echo "=== Mole Initial Server Setup ==="
echo "ROOT: $ROOT"
echo "SSH:  $SSH_HOST"
echo

echo "[1/4] Create directories"
ssh -o StrictHostKeyChecking=no "$SSH_HOST" "
  sudo mkdir -p '$REMOTE_BASE/certbot' '$REMOTE_BASE/releases' '$REMOTE_BASE/repo'
  sudo chown -R admin:admin '$REMOTE_BASE'
"

echo "[2/4] Sync repo assets"
rsync -az --delete \
  -e "ssh -o StrictHostKeyChecking=no" \
  --exclude '.git' \
  --exclude 'target' \
  --exclude 'frontend/dist' \
  --exclude '.env.local' \
  --exclude '.env.*.local' \
  --exclude 'node_modules' \
  "$ROOT/" "$SSH_HOST:$REMOTE_BASE/repo/"

echo "[3/4] Install bootstrap nginx config"
ssh -o StrictHostKeyChecking=no "$SSH_HOST" "
  sudo cp '$REMOTE_BASE/repo/infra/nginx/molefinance.net.bootstrap.conf' /etc/nginx/conf.d/molefinance.net.conf
  sudo nginx -t
  sudo systemctl reload nginx
"

echo "[4/4] Done"
echo "Next:"
echo "  1) ssh $SSH_HOST 'sudo certbot certonly --webroot -w $REMOTE_BASE/certbot -d molefinance.net -d www.molefinance.net'"
echo "  2) ssh $SSH_HOST 'sudo cp $REMOTE_BASE/repo/infra/nginx/molefinance.net.conf /etc/nginx/conf.d/molefinance.net.conf && sudo nginx -t && sudo systemctl reload nginx'"
echo "  3) bash deploy/scripts/mole-ecs-deploy-ui.sh"
