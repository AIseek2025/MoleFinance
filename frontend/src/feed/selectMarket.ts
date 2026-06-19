// Wave 19 — pick a single market from a multi-market feed snapshot.
//
// `MultiMarketFeedAdapter.aggregate()` mirrors the FIRST configured
// market's data into `FeedSnapshot.indexer / .keeper / .market` for
// wave-17 backward compatibility. When the operator has selected a
// DIFFERENT market in the wave-19 `MarketSelector`, we want every
// panel (`TraderPanel`, `IndexerPanel`, `KeeperPanel`) to render
// data for that selection without touching the panels themselves.
//
// `selectActiveMarketSnapshot(feed, symbol)` returns either:
//   - the input feed unchanged when:
//       * `feed.marketsView` is absent (single-market path), OR
//       * `symbol` is empty or not present in `marketsView`, OR
//       * the entry has no decoded data yet (so the caller falls
//         back to the wave-17 mirror in `feed.indexer`).
//   - a NEW `FeedSnapshot` whose `indexer.market / .subPools /
//     .dormantBuckets / .slot / .projectedRecoveryOutstanding...`
//     reflect the selected entry's decoded shapes.
//
// Wave 20 — extends the rewrite to:
//   * `feed.positions` filtered by `marketPdaHex` (positions tagged
//     with the active market's PDA come through; positions
//     missing the tag stay in the list as a back-compat for
//     single-market test fixtures).
//   * `feed.keeper` swapped to the active market's per-market
//     `KeeperState` when `MarketViewEntry.keeperState` is present
//     (multi-market keeper-bot publishes per-market metrics);
//     otherwise we keep the global `feed.keeper` as wave-19 did.
//
// The keeper view also flips when the selected market is paused
// (so `KeeperPanel` doesn't lie when the operator picks a paused
// market — the wave-17 banner already does this for the primary).

import type { FeedSnapshot, KeeperState, PositionSummary } from "../types";

export function selectActiveMarketSnapshot(
  feed: FeedSnapshot,
  symbol: string,
): FeedSnapshot {
  if (!feed.marketsView || symbol === "") return feed;
  const entry = feed.marketsView.entries.get(symbol);
  if (!entry) return feed;
  // No decoded data yet — keep the wave-17 mirror so the panels
  // still render something useful.
  if (!entry.marketSummary) return feed;
  const indexer = {
    slot: entry.indexerSlot ?? feed.indexer.slot,
    market: entry.marketSummary,
    subPools: entry.subPools ?? [],
    dormantBuckets: entry.dormantBuckets ?? [],
    pendingInitHints: feed.indexer.pendingInitHints,
    projectedRecoveryOutstandingMicroUsdc:
      entry.projectedRecoveryOutstandingMicroUsdc ?? 0n,
  };
  // Wave 20 — per-market keeper state takes precedence over the
  // global mirror; falls back to the global state with a
  // paused-flip when the entry has no per-market metrics yet.
  const baseKeeper = entry.keeperState ?? feed.keeper;
  const keeper: KeeperState = {
    ...baseKeeper,
    status: entry.marketSummary.paused ? "paused" : baseKeeper.status,
  };
  // Wave 20 — filter positions by owning market.
  const positions = filterPositionsByMarket(feed.positions, entry.marketPdaHex);
  return {
    ...feed,
    indexer,
    keeper,
    positions,
  };
}

/**
 * Wave 20 — pure helper exposed for unit tests. Returns positions
 * that either:
 *   1. Carry `marketPdaHex` matching `target`, OR
 *   2. Carry no `marketPdaHex` at all (untagged → kept for
 *      back-compat with wave-9..18 single-market mocks).
 *
 * Positions tagged with a DIFFERENT `marketPdaHex` are dropped.
 */
export function filterPositionsByMarket(
  positions: readonly PositionSummary[],
  target: string,
): PositionSummary[] {
  return positions.filter((p) => {
    if (p.marketPdaHex === undefined) return true;
    return p.marketPdaHex === target;
  });
}
