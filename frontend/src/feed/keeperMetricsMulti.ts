// Wave 22 — parse keeper-bot `/metrics-multi` JSON into
// `KeeperState` per market symbol. The wave-21 backend emits a
// stable camelCase object via `KeeperMetrics::render_json_snapshot`;
// this module is the frontend mirror so `MarketViewEntry.keeperState`
// can be filled without Prometheus grammar scraping.

import type {
  FeedSnapshot,
  KeeperState,
  MarketViewEntry,
  MultiMarketView,
} from "../types";

/** Raw metrics object from `/metrics-multi` (wave-21 wire format). */
export interface MetricsMultiRawMetrics {
  ticksTotal: number;
  actionsSubmittedTotal: number;
  actionsFailedTotal: number;
  actionsSkippedTotal: number;
  initHintsRecordedTotal?: number;
  snapshotErrorsTotal?: number;
  lastTickDurationMs: number;
  volSamples: number;
  lastInitHints?: number;
  lastActionsPlanned?: number;
  upSinceUnixSecs?: number;
  walletBalanceLamports: number;
  appliedVolMilli: number | null;
  leaderStatus: "leader" | "standby" | "unknown" | string;
}

export interface MetricsMultiEntry {
  market: string;
  metrics: MetricsMultiRawMetrics;
}

/**
 * Parse the `/metrics-multi` response body into typed entries.
 * Throws on malformed JSON or missing required fields.
 */
export function parseMetricsMultiJson(text: string): MetricsMultiEntry[] {
  const parsed: unknown = JSON.parse(text);
  if (!Array.isArray(parsed)) {
    throw new Error("metrics-multi: expected JSON array");
  }
  const out: MetricsMultiEntry[] = [];
  for (let i = 0; i < parsed.length; i += 1) {
    const row = parsed[i];
    if (typeof row !== "object" || row === null) {
      throw new Error(`metrics-multi[${i}]: expected object`);
    }
    const market = (row as { market?: unknown }).market;
    const metrics = (row as { metrics?: unknown }).metrics;
    if (typeof market !== "string" || market === "") {
      throw new Error(`metrics-multi[${i}]: missing market string`);
    }
    if (typeof metrics !== "object" || metrics === null) {
      throw new Error(`metrics-multi[${i}]: missing metrics object`);
    }
    out.push({
      market,
      metrics: metrics as MetricsMultiRawMetrics,
    });
  }
  return out;
}

/** Map wave-21 JSON metrics into the wave-12 `KeeperState` shape. */
export function metricsJsonToKeeperState(raw: MetricsMultiRawMetrics): KeeperState {
  const appliedVol =
    raw.appliedVolMilli === null ? null : raw.appliedVolMilli / 1000;
  const status: KeeperState["status"] =
    appliedVol === null && raw.volSamples < 3
      ? "warming_up"
      : "running";
  return {
    status,
    metrics: {
      tickSlot: raw.ticksTotal,
      appliedVol,
      volSamples: raw.volSamples,
      cumulative: {
        submitted: raw.actionsSubmittedTotal,
        failed: raw.actionsFailedTotal,
        skipped: raw.actionsSkippedTotal,
      },
      recent: {
        submitted: raw.lastActionsPlanned ?? 0,
        failed: 0,
        skipped: 0,
        durationMs: raw.lastTickDurationMs,
      },
      walletBalanceSol: raw.walletBalanceLamports / 1_000_000_000,
    },
    predictions: [],
    recentSignatures: [],
  };
}

/** Build a symbol → `KeeperState` map from parsed entries. */
export function keeperStateMapFromMetricsMulti(
  entries: readonly MetricsMultiEntry[],
): Map<string, KeeperState> {
  const out = new Map<string, KeeperState>();
  for (const e of entries) {
    out.set(e.market, metricsJsonToKeeperState(e.metrics));
  }
  return out;
}

/**
 * Wave 22 — merge polled `/metrics-multi` keeper states into a
 * `FeedSnapshot`. Multi-market path fills `marketsView.entries[].keeperState`;
 * single-market path overwrites `feed.keeper` when the symbol matches
 * `feed.indexer.market.symbol`.
 */
export function mergeKeeperMetricsIntoFeed(
  feed: FeedSnapshot,
  byMarket: ReadonlyMap<string, KeeperState>,
): FeedSnapshot {
  if (byMarket.size === 0) return feed;

  if (feed.marketsView) {
    const entries = new Map<string, MarketViewEntry>();
    for (const [symbol, entry] of feed.marketsView.entries) {
      const ks = byMarket.get(symbol);
      entries.set(symbol, ks !== undefined ? { ...entry, keeperState: ks } : entry);
    }
    const primary = feed.indexer.market.symbol;
    const primaryKeeper = byMarket.get(primary);
    return {
      ...feed,
      ...(primaryKeeper !== undefined && { keeper: primaryKeeper }),
      marketsView: { entries } as MultiMarketView,
    };
  }

  const symbol = feed.indexer.market.symbol;
  const direct = byMarket.get(symbol);
  if (direct !== undefined) {
    return { ...feed, keeper: direct };
  }
  if (byMarket.size === 1) {
    const only = byMarket.values().next().value;
    if (only) return { ...feed, keeper: only };
  }
  return feed;
}
