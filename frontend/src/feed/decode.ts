// Wave 19 — shared `OnchainX` → `XSummary` converters.
//
// Both `WebSocketFeedAdapter` (wave 14, single-market) and
// `MultiMarketFeedAdapter` (wave 18, multi-market) need to turn
// the raw Borsh-decoded shapes from `decoder/onchain.ts` into the
// summary types the React panels consume. Wave 18 left the
// converter inline in `WebSocketFeedAdapter`; wave 19 lifts it to
// this module so the multi-market adapter can reuse the same
// logic without copy-paste drift.

import type { Buffer } from "buffer";
import { PublicKey } from "@solana/web3.js";

import type {
  OnchainDormantBucket,
  OnchainMarket,
  OnchainPosition,
  OnchainSubPool,
} from "../decoder/onchain";
import type {
  DormantBucketSummary,
  MarketSummary,
  PositionSummary,
  Pubkey32,
  SubPoolSummary,
} from "../types";

/** On-chain `Position.status`: 0=Open, 1=Dormant, 2=Closed. */
export const POSITION_STATUS_CLOSED = 2;

/** Whether a decoded position should appear in trader-panel rosters. */
export function isDisplayablePosition(status: number): boolean {
  return status !== POSITION_STATUS_CLOSED;
}

/**
 * Convert a decoded `OnchainMarket` (Borsh shape) into the
 * trader-panel-friendly `MarketSummary`. `marketPdaHex` is the
 * 32-byte hex string of the `Market` PDA pubkey.
 *
 * `currentSlot` is the cluster slot the caller is using to age
 * the snapshot; multi-market path sets this from `getSlot()`,
 * single-market wave-14 path leaves it at 0.
 */
export function onchainMarketToSummary(
  market: OnchainMarket,
  marketPdaHex: string,
  currentSlot = 0,
): MarketSummary {
  return {
    pubkey: { hex: marketPdaHex },
    symbol: bufferToAscii(market.symbol),
    schemaVersion: market.schema_version,
    paused: market.paused,
    pausedGlobally: market.paused,
    frozenNewPosition: market.frozen_new_position,
    midPriceMicro: market.price_tick,
    lastOracleSlot: 0,
    currentSlot,
    currentTotalPrincipal: market.current_total_principal,
    currentTotalNotional: market.current_total_notional,
  };
}

/**
 * Wave 22 — convert a decoded `OnchainPosition` into the trader-
 * panel `PositionSummary`. Lifts `position.market.hex` into
 * `marketPdaHex` so wave-20 `selectActiveMarketSnapshot` can filter
 * by active market on live websocket data.
 */
export function onchainPositionToSummary(
  pos: OnchainPosition,
  subPoolId: number,
): PositionSummary {
  return {
    owner: pos.owner,
    subPoolId,
    direction: pos.direction_is_long ? "Long" : "Short",
    qty: pos.active_shares,
    collateral: pos.principal,
    openedAt: Number(pos.opened_at),
    marketPdaHex: pos.market.hex,
  };
}

/** Convert a decoded `OnchainSubPool` to the panel-friendly summary. */
export function onchainSubPoolToSummary(
  sp: OnchainSubPool,
  pubkeyHex: string,
): SubPoolSummary {
  return {
    id: sp.sub_pool_id,
    pubkey: { hex: pubkeyHex },
    totalOpenLongQty: sp.long_active_shares,
    totalOpenShortQty: sp.short_active_shares,
    longCollateral: sp.long_pool_equity,
    shortCollateral: sp.short_pool_equity,
    dormantInventory: {
      Long: sp.long_dormant_bucket_count,
      Short: sp.short_dormant_bucket_count,
    },
  };
}

/**
 * Convert a decoded `OnchainDormantBucket` to the panel-friendly
 * summary. `subPoolId` is supplied by the caller because the
 * raw Borsh layout only carries the parent `sub_pool` pubkey
 * (the routing layer maps that to a sub-pool id before calling).
 */
export function onchainDormantBucketToSummary(
  bucket: OnchainDormantBucket,
  subPoolId: number,
): DormantBucketSummary {
  return {
    subPoolId,
    direction: bucket.direction_is_long ? "Long" : "Short",
    tick: Number(bucket.zero_price_tick),
    totalShares: bucket.total_recovery_shares,
    pendingRecoveryMicroUsdc: bucket.accrued_value,
    readyToClose:
      bucket.position_count === 0n &&
      bucket.last_applied_index >= 0n &&
      bucket.total_recovery_shares === 0n,
  };
}

/**
 * Convert a `Buffer | Uint8Array` view of a 32-byte pubkey into a
 * `Pubkey32` (hex shape). Defensive: accepts the canonical 32-byte
 * width and any sub-buffer of that width.
 */
export function pubkey32FromBytes(bytes: Uint8Array): Pubkey32 {
  let s = "";
  for (let i = 0; i < bytes.length; i += 1) {
    s += (bytes[i] ?? 0).toString(16).padStart(2, "0");
  }
  return { hex: s };
}

/** Convert a base58 pubkey (e.g. from `accountId.toBase58()`) to `Pubkey32`. */
export function pubkey32FromBase58(b58: string): Pubkey32 {
  const buf = new PublicKey(b58).toBytes();
  return pubkey32FromBytes(buf);
}

function bufferToAscii(buf: Buffer): string {
  // `OnchainMarket.symbol` is 16 zero-padded bytes; strip trailing NULs.
  const end = buf.indexOf(0);
  return (end < 0 ? buf : buf.subarray(0, end)).toString("ascii");
}

/** Compare two byte arrays for equality. */
export function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}
