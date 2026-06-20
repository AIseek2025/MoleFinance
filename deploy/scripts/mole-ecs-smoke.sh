#!/usr/bin/env bash
set -euo pipefail

SSH_HOST="${MOLE_SSH_HOST:-admin@8.218.209.218}"
REMOTE_BASE="${MOLE_REMOTE_BASE:-/var/www/molefinance}"
PUBLIC_URL="${MOLE_PUBLIC_URL:-https://molefinance.net}"
WWW_URL="${MOLE_WWW_URL:-https://www.molefinance.net}"

echo "=== Mole ECS Smoke ==="

echo "[1/4] HTTPS headers"
curl -I --max-time 20 "$PUBLIC_URL"

echo "[2/4] WWW redirect"
curl -I --max-time 20 "$WWW_URL"

echo "[3/4] Homepage content"
curl -s --max-time 20 "$PUBLIC_URL" | grep -q "MoleOption Console"
echo "  homepage token OK"

echo "[4/4] Remote current release"
ssh -o StrictHostKeyChecking=no "$SSH_HOST" "
  echo CURRENT=\$(readlink -f '$REMOTE_BASE/current')
"

echo "=== Smoke passed ==="
