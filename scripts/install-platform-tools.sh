#!/usr/bin/env bash
# MoleOption — robust platform-tools installer (wave 30).
#
# `cargo build-sbf`'s built-in downloader has NO resume support, so on a
# flaky / GFW'd link the tarball truncates (`IncompleteBody`) and never
# completes. This script downloads the SAME official tarball with:
#   - curl resume (-C -) in a retry loop → truncations just continue
#   - multiple GitHub mirrors (gh-proxy / ghfast / moeyy) as fallback
#   - integrity check (tar -tf) BEFORE extracting (never extract a
#     partial file — that is what left an empty rust/ dir before)
# then places it where `cargo build-sbf` expects, so the next build
# finds it and skips downloading.
#
# Respects HTTPS_PROXY / ALL_PROXY if you have a local proxy (Clash etc.)
# — that alone usually fixes the truncation; just export it and re-run
# `cargo build-sbf`. This script is the no-proxy fallback.
#
# Usage: scripts/install-platform-tools.sh [version]   (default v1.52)
set -euo pipefail

VERSION="${1:-v1.52}"
CACHE_BASE="$HOME/.cache/solana/$VERSION"
CACHE="$CACHE_BASE/platform-tools"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64|Darwin-aarch64) ASSET="platform-tools-osx-aarch64.tar.bz2" ;;
  Darwin-x86_64)               ASSET="platform-tools-osx-x86_64.tar.bz2" ;;
  Linux-x86_64)                ASSET="platform-tools-linux-x86_64.tar.bz2" ;;
  Linux-aarch64|Linux-arm64)   ASSET="platform-tools-linux-aarch64.tar.bz2" ;;
  *) echo "unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

GH="https://github.com/anza-xyz/platform-tools/releases/download/$VERSION/$ASSET"
MIRRORS=(
  "$GH"
  "https://gh-proxy.com/$GH"
  "https://ghfast.top/$GH"
  "https://github.moeyy.xyz/$GH"
)

TMP="$CACHE_BASE/dl-$ASSET"
mkdir -p "$CACHE_BASE"

log() { printf '\033[36m[pt-install]\033[0m %s\n' "$*"; }

integrity_ok() { [[ -s "$TMP" ]] && tar -tf "$TMP" >/dev/null 2>&1; }

if integrity_ok; then
  log "a complete tarball is already cached at $TMP — skipping download"
else
  ok=0
  for url in "${MIRRORS[@]}"; do
    log "source: $url"
    for attempt in $(seq 1 30); do
      # -C - resumes; on a fully-downloaded file curl returns 416 which
      # we ignore, then the integrity check below confirms completeness.
      curl -L --fail-early --connect-timeout 30 --retry 5 --retry-delay 3 \
        --retry-all-errors -C - -o "$TMP" "$url" || true
      if integrity_ok; then ok=1; break; fi
      log "  incomplete — resuming (attempt $attempt)…"
      sleep 2
    done
    [[ $ok -eq 1 ]] && break
    log "source failed, trying next mirror…"
  done
  [[ $ok -eq 1 ]] || { echo "[pt-install] all mirrors failed — see proxy option in script header" >&2; exit 1; }
fi

log "verified tarball OK — extracting to $CACHE"
# Extract directly into the cache (no temp copy) to keep peak disk usage
# low. Handle both layouts: top-level rust/llvm, or a wrapping
# platform-tools/ dir.
rm -rf "$CACHE"; mkdir -p "$CACHE"
tar -xf "$TMP" -C "$CACHE"
if [[ ! -d "$CACHE/rust/lib" && -d "$CACHE/platform-tools/rust/lib" ]]; then
  shopt -s dotglob
  mv "$CACHE/platform-tools/"* "$CACHE/"
  rmdir "$CACHE/platform-tools"
  shopt -u dotglob
fi
[[ -d "$CACHE/rust/lib" ]] || { echo "[pt-install] extracted tree missing rust/lib" >&2; exit 1; }

# CRITICAL (macOS / Apple Silicon): the tarball is downloaded via a
# browser/proxy, so its binaries inherit `com.apple.quarantine`. When
# `cargo-build-sbf` executes the quarantined, ad-hoc-signed `rustc`,
# Gatekeeper SIGKILLs it AND macOS removes it as "damaged" — which is
# why the toolchain mysteriously self-destructs mid-build. Strip the
# attribute per-file (skip symlinks so a dangling lldb symlink can't
# abort the whole pass).
if [[ "$(uname -s)" == "Darwin" ]]; then
  log "stripping com.apple.quarantine from toolchain binaries…"
  find "$CACHE" -type f -exec xattr -d com.apple.quarantine {} \; 2>/dev/null || true
fi

log "done ✓  platform-tools $VERSION ready at $CACHE"
log "now run:  scripts/deploy-devnet.sh build"
