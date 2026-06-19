// Wave 18 — KL-09: Decode + print every market's `KeeperLeaderLock`
// in one shot from a multi-market TOML registry.
//
// Runbook reference: § 6.6 (multi-market ops) — operators run this
// from incident response when they need a single command that shows
// "is every market's leader healthy right now?". The single-market
// `keeper-leader-show.ts` stays for per-market deep dives; this
// script is the situational-awareness pane.
//
// Output is a JSON object keyed by symbol, with a top-level
// `worstStatus` field that AlertManager can grep for. `--human`
// emits a colour-light summary table instead.
//
// Usage:
//
//   keeper-leader-show-all --markets ./markets.toml [--rpc <url>]
//                          [--expected-only] [--human]

import {
  connectionFromArgs,
  decodeKeeperLeaderLockAccount,
  loadMarketsToml,
  maybeFlag,
  parseFlags,
  shortenHex,
  type MarketRegistryEntry,
} from "./lib.js";

interface MarketStatus {
  symbol: string;
  pda: string;
  initialized: boolean;
  hasLeader?: boolean;
  currentLeaderHex?: string;
  currentLeaderShort?: string;
  expectedLeaderShort?: string;
  expectedMatch?: boolean;
  lastHeartbeatSlot?: string;
  takeoverThresholdSlots?: string;
  elapsedSlots?: number;
  stale?: boolean;
  /**
   * Worst observation per market for AlertManager: matches the
   * wave-17 banner state machine plus a `mismatch` bucket for the
   * wave-18 `expected_leader` injection. Severity climbs as:
   *   pass < uninitialised < unowned < stale < mismatch
   */
  status:
    | "pass"
    | "uninitialised"
    | "unowned"
    | "stale"
    | "mismatch"
    | "error";
  message?: string;
}

async function main(): Promise<void> {
  const flags = parseFlags(process.argv.slice(2));
  if (flags.has("help") || flags.has("h")) {
    printUsage();
    return;
  }
  const marketsPath = maybeFlag(flags, "markets");
  if (!marketsPath) {
    throw new Error(
      "missing required flag --markets <path>. See keeper-leader-show-all --help.",
    );
  }
  const expectedOnly = flags.has("expected-only");
  const human = flags.has("human");
  const conn = connectionFromArgs(maybeFlag(flags, "rpc"));
  const entries = loadMarketsToml(marketsPath);
  const filtered = expectedOnly
    ? entries.filter((e) => e.expectedLeader !== undefined)
    : entries;
  if (filtered.length === 0) {
    throw new Error(
      expectedOnly
        ? "no markets in registry have expected_leader set"
        : "markets.toml has no entries",
    );
  }
  const currentSlot = await conn.getSlot("confirmed");
  const results: MarketStatus[] = [];
  for (const entry of filtered) {
    results.push(await probeMarket(conn, entry, currentSlot));
  }
  const worstStatus = computeWorstStatus(results);
  if (human) {
    printHuman(results, worstStatus, currentSlot);
  } else {
    console.log(
      JSON.stringify(
        { worstStatus, currentSlot, markets: results },
        null,
        2,
      ),
    );
  }
  process.exit(exitCodeForStatus(worstStatus));
}

async function probeMarket(
  conn: Awaited<ReturnType<typeof connectionFromArgs>>,
  entry: MarketRegistryEntry,
  currentSlot: number,
): Promise<MarketStatus> {
  const base: MarketStatus = {
    symbol: entry.symbol,
    pda: entry.lockPda.toBase58(),
    initialized: false,
    status: "uninitialised",
  };
  let info;
  try {
    info = await conn.getAccountInfo(entry.lockPda);
  } catch (e) {
    return {
      ...base,
      status: "error",
      message: `getAccountInfo failed: ${e instanceof Error ? e.message : String(e)}`,
    };
  }
  if (!info) {
    return {
      ...base,
      message:
        "PDA does not exist on chain — run keeper-leader-init.ts (KL-01)",
    };
  }
  let view;
  try {
    view = decodeKeeperLeaderLockAccount(info.data);
  } catch (e) {
    return {
      ...base,
      initialized: true,
      status: "error",
      message: `decode failed: ${e instanceof Error ? e.message : String(e)}`,
    };
  }
  const elapsed = view.hasLeader
    ? Math.max(0, currentSlot - Number(view.lastHeartbeatSlot))
    : 0;
  const stale =
    view.hasLeader && BigInt(elapsed) >= view.takeoverThresholdSlots;
  let status: MarketStatus["status"];
  if (!view.hasLeader) {
    status = "unowned";
  } else if (stale) {
    status = "stale";
  } else {
    status = "pass";
  }
  let expectedMatch: boolean | undefined;
  let expectedLeaderShort: string | undefined;
  if (entry.expectedLeader && view.hasLeader) {
    const expectedBuf = Buffer.from(entry.expectedLeader.toBytes());
    expectedLeaderShort = shortenHex(expectedBuf);
    expectedMatch = expectedBuf.equals(view.currentLeader);
    if (!expectedMatch) {
      // Mismatch is loudest → overrides stale/pass below.
      status = "mismatch";
    }
  }
  return {
    symbol: entry.symbol,
    pda: entry.lockPda.toBase58(),
    initialized: true,
    hasLeader: view.hasLeader,
    currentLeaderHex: view.currentLeader.toString("hex"),
    currentLeaderShort: shortenHex(view.currentLeader),
    ...(expectedLeaderShort !== undefined && { expectedLeaderShort }),
    ...(expectedMatch !== undefined && { expectedMatch }),
    lastHeartbeatSlot: view.lastHeartbeatSlot.toString(),
    takeoverThresholdSlots: view.takeoverThresholdSlots.toString(),
    elapsedSlots: elapsed,
    stale,
    status,
  };
}

function computeWorstStatus(results: MarketStatus[]): MarketStatus["status"] {
  const rank: Record<MarketStatus["status"], number> = {
    pass: 0,
    uninitialised: 1,
    unowned: 2,
    stale: 3,
    mismatch: 4,
    error: 4,
  };
  let worst: MarketStatus["status"] = "pass";
  for (const r of results) {
    if (rank[r.status] > rank[worst]) worst = r.status;
  }
  return worst;
}

/**
 * Wave 18 exit code matrix (lines up with `ops-toolkit` Rust
 * tiers): 0 pass, 1 uninitialised/unowned (P3-ish — non-paging),
 * 2 stale (P2 — page during business hours),
 * 3 mismatch / error (P1 — page now).
 */
function exitCodeForStatus(s: MarketStatus["status"]): number {
  switch (s) {
    case "pass":
      return 0;
    case "uninitialised":
    case "unowned":
      return 1;
    case "stale":
      return 2;
    case "mismatch":
    case "error":
      return 3;
  }
}

function printHuman(
  results: MarketStatus[],
  worst: MarketStatus["status"],
  currentSlot: number,
): void {
  console.log(`Cluster slot: ${currentSlot}`);
  console.log(`Worst status: ${worst}`);
  console.log("");
  const headers = ["symbol", "status", "holder", "expected", "elapsed"];
  const rows = results.map((r) => [
    r.symbol,
    r.status,
    r.currentLeaderShort ?? "—",
    r.expectedLeaderShort ?? "—",
    r.elapsedSlots !== undefined ? `${r.elapsedSlots}` : "—",
  ]);
  const widths = headers.map((h, i) =>
    Math.max(h.length, ...rows.map((r) => r[i]!.length)),
  );
  const line = (cells: string[]): string =>
    cells.map((c, i) => c.padEnd(widths[i]!)).join("  ");
  console.log(line(headers));
  console.log(widths.map((w) => "-".repeat(w)).join("  "));
  for (const r of rows) {
    console.log(line(r));
  }
}

function printUsage(): void {
  console.log(`Usage: keeper-leader-show-all [flags]

Required:
  --markets <path>         Path to markets.toml (wave-18 multi-market registry)

Optional:
  --rpc           <url>    (or MOLE_RPC_URL env)
  --expected-only          Only probe markets with expected_leader set
  --human                  Pretty-print summary table instead of JSON
  --help                   Show this help and exit

Exit codes:
  0  every market PASS
  1  any market uninitialised / unowned (P3-ish)
  2  any market stale (P2)
  3  any market mismatch / error (P1)
`);
}

main().catch((e) => {
  console.error(
    `[keeper-leader-show-all] fatal: ${e instanceof Error ? e.message : e}`,
  );
  process.exit(1);
});
