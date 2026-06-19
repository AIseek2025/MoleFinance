// Wave 18 — frontend-side multi-market config parser.
//
// Mirrors `crates/keeper-rpc/src/market_registry.rs`. The Rust
// registry is the source of truth for keeper-bot + ops-toolkit;
// the frontend doesn't have FS access at runtime, so we ship the
// same logical schema via a Vite env var (`VITE_MARKETS`) — a
// JSON array. The shape is intentionally narrower than the Rust
// TOML: the frontend only needs the lookup keys + expected_leader
// hex; lock_pda is re-derived client-side from `program_id +
// market_pda` so the operator only has to update one field per
// market across both sides.
//
// ## Why JSON in an env var?
//
// - Vite inlines `import.meta.env` at build time; ops can ship a
//   single `.env.production` file that pins all markets in one
//   place.
// - JSON parses with `JSON.parse` (no extra dependency) and
//   round-trips cleanly with the Rust TOML by hand.
// - We deliberately don't ship a runtime fetch of the same TOML
//   the keeper bot reads — that would couple the frontend's CSP
//   to ops infrastructure. A build-time bake-in is simpler.

import { PublicKey } from "@solana/web3.js";

import type { MultiMarketEntry } from "./feed/multiMarketAdapter";

/** Wave 18 — one market entry as the JSON env var carries it. */
export interface MarketConfigEntry {
  symbol: string;
  programId: string;
  marketPda: string;
  /** Optional — overrides the derived `keeper_leader_lock` PDA. */
  lockPda?: string;
  /**
   * Optional 32-byte hex (no `0x`, lowercase) of the operator-
   * authoritative expected leader. Surfaced into the
   * `LeaderLockGrid` for mismatch flagging.
   */
  expectedLeader?: string;
}

/** Wave 18 — parse output (env-var → adapter input). */
export interface ParsedMarketsConfig {
  /** Adapter-ready entries (PublicKey instances + symbol). */
  adapter: MultiMarketEntry[];
  /** Per-symbol expected_leader hex map (for the grid). */
  expectedLeaders: Map<string, string>;
  /** Original config entries (for diagnostics / future panels). */
  raw: MarketConfigEntry[];
}

/**
 * Parse a JSON-encoded multi-market config. `null`/`undefined`/
 * empty string return `null` so the caller can fall through to
 * the single-market path.
 *
 * Throws `Error` only on malformed JSON or schema violations the
 * caller's UI must surface; missing config is NOT an error.
 */
export function parseMarketsConfig(
  raw: string | undefined | null,
): ParsedMarketsConfig | null {
  if (raw === undefined || raw === null) return null;
  const trimmed = raw.trim();
  if (trimmed.length === 0) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch (e) {
    throw new Error(`VITE_MARKETS: invalid JSON — ${(e as Error).message}`);
  }
  if (!Array.isArray(parsed)) {
    throw new Error("VITE_MARKETS: expected a JSON array of market entries");
  }
  const entries: MarketConfigEntry[] = [];
  const seen = new Set<string>();
  for (let i = 0; i < parsed.length; i += 1) {
    const item = parsed[i];
    if (
      typeof item !== "object" ||
      item === null ||
      typeof (item as Record<string, unknown>).symbol !== "string" ||
      typeof (item as Record<string, unknown>).programId !== "string" ||
      typeof (item as Record<string, unknown>).marketPda !== "string"
    ) {
      throw new Error(
        `VITE_MARKETS: entry #${i} missing one of {symbol, programId, marketPda}`,
      );
    }
    const e = item as MarketConfigEntry;
    if (seen.has(e.symbol)) {
      throw new Error(`VITE_MARKETS: duplicate symbol '${e.symbol}'`);
    }
    if (e.symbol.length === 0 || e.symbol.length > 16) {
      throw new Error(
        `VITE_MARKETS: symbol '${e.symbol}' must be 1..16 ASCII bytes`,
      );
    }
    if (e.expectedLeader !== undefined && !/^[0-9a-fA-F]{64}$/.test(e.expectedLeader)) {
      throw new Error(
        `VITE_MARKETS: entry '${e.symbol}' expectedLeader must be 64 hex chars`,
      );
    }
    seen.add(e.symbol);
    entries.push(e);
  }
  if (entries.length === 0) return null;
  const adapter: MultiMarketEntry[] = [];
  const expectedLeaders = new Map<string, string>();
  for (const e of entries) {
    let programId: PublicKey;
    let marketPda: PublicKey;
    try {
      programId = new PublicKey(e.programId);
      marketPda = new PublicKey(e.marketPda);
    } catch (err) {
      throw new Error(
        `VITE_MARKETS: entry '${e.symbol}' programId/marketPda is not valid base58 — ${(err as Error).message}`,
      );
    }
    let lockPda: PublicKey;
    if (e.lockPda !== undefined && e.lockPda.length > 0) {
      try {
        lockPda = new PublicKey(e.lockPda);
      } catch (err) {
        throw new Error(
          `VITE_MARKETS: entry '${e.symbol}' lockPda is not valid base58 — ${(err as Error).message}`,
        );
      }
    } else {
      const [derived] = PublicKey.findProgramAddressSync(
        [Buffer.from("keeper_leader_lock"), marketPda.toBytes()],
        programId,
      );
      lockPda = derived;
    }
    adapter.push({ symbol: e.symbol, marketPda, lockPda });
    if (e.expectedLeader !== undefined && e.expectedLeader.length > 0) {
      expectedLeaders.set(e.symbol, e.expectedLeader.toLowerCase());
    }
  }
  return { adapter, expectedLeaders, raw: entries };
}
