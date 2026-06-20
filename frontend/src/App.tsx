import { useEffect, useMemo, useState } from "react";
import type { JSX } from "react";

import { LandingPage } from "./landing/LandingPage";
import { TradeView } from "./trade/TradeView";
import { OverviewPanel } from "./panels/OverviewPanel";
import { TraderPanel } from "./panels/TraderPanel";
import { IndexerPanel } from "./panels/IndexerPanel";
import { KeeperPanel } from "./panels/KeeperPanel";
import { LeaderLockBanner } from "./panels/LeaderLockBanner";
import { LeaderLockGrid } from "./panels/LeaderLockGrid";
import { MarketSelector } from "./panels/MarketSelector";
import { ProberHealthPanel } from "./panels/ProberHealthPanel";
import { decodeKeeperLeaderLockBytes } from "./tx/wasmBuilder";
import type { KeeperLeaderLockView } from "./tx/wasmBuilder";
import { PublicKey } from "@solana/web3.js";

import {
  MockFeedAdapter,
  WebSocketFeedAdapter,
  adapterKindFromUrl,
  useFeed,
} from "./feed";
import { MultiMarketFeedAdapter } from "./feed/multiMarketAdapter";
import { selectActiveMarketSnapshot } from "./feed/selectMarket";
import { mergeKeeperMetricsIntoFeed } from "./feed/keeperMetricsMulti";
import type { FeedAdapter } from "./feed";
import { MOLE_ACCOUNT_DISCRIMINATORS } from "./decoder/discriminators";
import { parseMarketsConfig } from "./marketRegistry";
import { selectWalletAdapter, WindowWalletAdapter } from "./wallet";
import { useActiveMarket } from "./useActiveMarket";
import { useKeeperMetricsMulti } from "./useKeeperMetricsMulti";
import { useProberSnapshot } from "./useProberSnapshot";
import type { WalletAdapter, WalletStatus } from "./wallet";
import { formatPubkey, formatSlot } from "./format";

type PanelId = "overview" | "trader" | "indexer" | "keeper";

/** Top-level screen: marketing landing → trading terminal → ops console. */
type Screen = "landing" | "trade" | "console";

function initialScreen(): Screen {
  if (typeof window === "undefined") return "landing";
  const v = new URLSearchParams(window.location.search).get("view");
  if (v === "trade") return "trade";
  if (v === "console") return "console";
  return "landing";
}

const TABS: { id: PanelId; label: string; description: string }[] = [
  {
    id: "overview",
    label: "Overview",
    description: "Protocol TVL, open interest, per-market health",
  },
  {
    id: "trader",
    label: "Trader",
    description: "Open / close positions, watch live price",
  },
  {
    id: "indexer",
    label: "Indexer State",
    description: "Sub-pool stats, dormant inventory, recovery outstanding",
  },
  {
    id: "keeper",
    label: "Keeper Console",
    description: "Loop metrics, vol estimator, action queue",
  },
];

interface AdapterBuild {
  adapter: FeedAdapter;
  /** Wave 18 — when present, App renders LeaderLockGrid instead of banner. */
  expectedLeaders?: Map<string, string>;
}

function buildAdapter(): AdapterBuild {
  const kind = adapterKindFromUrl();
  if (kind === "websocket") {
    // Wave 14 — read connection params from Vite env (`VITE_RPC_URL`,
    // `VITE_MOLE_PROGRAM_ID`, `VITE_MARKET_PDA`). Fall back to mock
    // when any of them is missing so the default `npm run dev` flow
    // keeps working without on-chain configuration.
    const env = (import.meta as unknown as {
      env?: Record<string, string | undefined>;
    }).env;
    const rpcUrl = env?.VITE_RPC_URL;
    // Wave 18 — multi-market opt-in via VITE_MARKETS (JSON array).
    // When present and parseable, take the multi-market path; the
    // single-market wave-17 wiring stays as a strict fallback so
    // operators with one market still get the banner UX.
    const marketsRaw = env?.VITE_MARKETS;
    if (rpcUrl && marketsRaw) {
      try {
        const cfg = parseMarketsConfig(marketsRaw);
        if (cfg && cfg.adapter.length > 0) {
          const programId = new PublicKey(cfg.raw[0]!.programId);
          return {
            adapter: new MultiMarketFeedAdapter({
              url: rpcUrl,
              programId,
              markets: cfg.adapter,
              // Wave 19 — wire the program-account stream so the
              // adapter can fan out sub-pool / dormant-bucket
              // updates per-market into `marketsView.entries`.
              discriminators: MOLE_ACCOUNT_DISCRIMINATORS,
              trackClusterSlot: true,
            }),
            expectedLeaders: cfg.expectedLeaders,
          };
        }
      } catch (e) {
        console.error(
          "[mole/frontend] VITE_MARKETS parse failed; falling back to single-market path —",
          e,
        );
      }
    }
    const programIdRaw = env?.VITE_MOLE_PROGRAM_ID;
    const marketPdaRaw = env?.VITE_MARKET_PDA;
    if (rpcUrl && programIdRaw && marketPdaRaw) {
      const programId = new PublicKey(programIdRaw);
      const marketPda = new PublicKey(marketPdaRaw);
      // Wave 17 — derive the keeper-leader-lock PDA so the live
      // adapter can subscribe to it and the LeaderLockBanner shows
      // real data instead of the wave-16 `view={null}` placeholder.
      const [keeperLeaderLockPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("keeper_leader_lock"), marketPda.toBytes()],
        programId,
      );
      return {
        adapter: new WebSocketFeedAdapter({
          url: rpcUrl,
          programId,
          marketPda,
          discriminators: MOLE_ACCOUNT_DISCRIMINATORS,
          keeperLeaderLockPda,
          trackClusterSlot: true,
        }),
      };
    }
    console.warn(
      "[mole/frontend] websocket feed requested but VITE_RPC_URL / " +
        "VITE_MOLE_PROGRAM_ID / VITE_MARKET_PDA env not set — " +
        "falling back to MockFeedAdapter",
    );
  }
  return { adapter: new MockFeedAdapter() };
}

export function App(): JSX.Element {
  const [screen, setScreen] = useState<Screen>(initialScreen);
  const [active, setActive] = useState<PanelId>("overview");
  const built = useMemo(() => buildAdapter(), []);
  const adapter = built.adapter;
  const expectedLeaders = built.expectedLeaders;
  // Wave 30 — pick a real browser wallet (Phantom / Backpack / Solflare)
  // when one is installed, else fall back to the offline mock adapter.
  // `?wallet=mock` forces the mock; `?wallet=<name>` forces a specific
  // installed wallet.
  const wallet = useMemo<WalletAdapter>(() => selectWalletAdapter(), []);
  const [walletStatus, setWalletStatus] = useState<WalletStatus>(wallet.status());
  const [walletPubkey, setWalletPubkey] = useState(wallet.pubkey());

  const { snapshot: rawFeed, status, adapterKind } = useFeed(adapter);
  const keeperByMarket = useKeeperMetricsMulti();
  const proberSnapshot = useProberSnapshot();

  // Wave 22 — overlay polled `/metrics-multi` JSON onto the RPC feed
  // before market selection so `MarketViewEntry.keeperState` and
  // single-market `feed.keeper` reflect live keeper-bot metrics.
  const feedWithKeeper = useMemo(() => {
    if (!rawFeed || !keeperByMarket || keeperByMarket.size === 0) {
      return rawFeed;
    }
    return mergeKeeperMetricsIntoFeed(rawFeed, keeperByMarket);
  }, [rawFeed, keeperByMarket]);

  // Wave 19 — derive the configured-market symbol list (sorted)
  // from the multi-market view. Empty when running single-market.
  const symbols = useMemo<string[]>(() => {
    if (!feedWithKeeper?.marketsView) return [];
    return Array.from(feedWithKeeper.marketsView.entries.keys()).sort();
  }, [feedWithKeeper?.marketsView]);
  const [activeSymbol, setActiveSymbol] = useActiveMarket(symbols);

  // Wave 19 — when the operator picks a non-primary market in the
  // selector, rewrite `feed.indexer / .keeper` to that market's
  // decoded view so every panel renders the right data WITHOUT
  // any panel-level changes (panels still consume `feed.indexer`).
  const feed = useMemo(() => {
    if (!feedWithKeeper) return feedWithKeeper;
    return selectActiveMarketSnapshot(feedWithKeeper, activeSymbol);
  }, [feedWithKeeper, activeSymbol]);

  // Wave 17 — decode the leader-lock raw bytes only when they
  // change (decode is wasm + Borsh, cheap but non-zero; React
  // would otherwise re-decode every parent render).
  const leaderLockView = useMemo<KeeperLeaderLockView | null>(() => {
    const bytes = feed?.keeperLeaderLockBytes;
    if (!bytes || bytes.length === 0) return null;
    try {
      return decodeKeeperLeaderLockBytes(bytes);
    } catch (e) {
      console.warn("[mole/frontend] keeper-leader-lock decode failed —", e);
      return null;
    }
  }, [feed?.keeperLeaderLockBytes]);

  useEffect(() => {
    const id = setInterval(() => {
      setWalletStatus(wallet.status());
      setWalletPubkey(wallet.pubkey());
    }, 250);
    // Wave 30 — for real browser wallets, attempt a silent reconnect on
    // load and react to out-of-band changes (account switch / extension
    // disconnect) via the provider event stream.
    let unsubscribe: (() => void) | undefined;
    if (wallet instanceof WindowWalletAdapter) {
      unsubscribe = wallet.onChange((s, pk) => {
        setWalletStatus(s);
        setWalletPubkey(pk);
      });
      void wallet.eagerConnect();
    }
    return () => {
      clearInterval(id);
      unsubscribe?.();
      if (wallet instanceof WindowWalletAdapter) wallet.dispose();
    };
  }, [wallet]);

  const onWalletConnect = async () => {
    try {
      await wallet.connect();
      setWalletStatus(wallet.status());
      setWalletPubkey(wallet.pubkey());
    } catch (e) {
      console.warn("[mole/frontend] wallet connect failed", e);
      setWalletStatus("error");
    }
  };

  const onWalletDisconnect = async () => {
    await wallet.disconnect();
    setWalletStatus(wallet.status());
    setWalletPubkey(wallet.pubkey());
  };

  const livePriceUsd =
    feed && feed.indexer.market.midPriceMicro != null
      ? Number(feed.indexer.market.midPriceMicro) / 1_000_000
      : null;

  // Marketing landing page — renders without a live feed so it loads
  // instantly even while the RPC adapter is still booting.
  if (screen === "landing") {
    return (
      <LandingPage
        onLaunch={() => setScreen("trade")}
        onConsole={() => setScreen("console")}
        livePriceUsd={livePriceUsd}
        liveSymbol={feed?.indexer.market.symbol ?? "SOL-PERP"}
      />
    );
  }

  // Hyperliquid-style trading terminal.
  if (screen === "trade") {
    if (!feed) {
      return (
        <div className="app">
          <div className="empty-state">
            <h2>正在连接行情…</h2>
            <p>等待 RPC feed 启动（状态：{status}）。</p>
            <button
              type="button"
              className="wallet-btn"
              onClick={() => setScreen("landing")}
            >
              返回首页
            </button>
          </div>
        </div>
      );
    }
    return (
      <TradeView
        feed={feed}
        wallet={wallet}
        walletStatus={walletStatus}
        {...(walletPubkey?.hex ? { walletPubkeyHex: walletPubkey.hex } : {})}
        onConnect={() => void onWalletConnect()}
        onDisconnect={() => void onWalletDisconnect()}
        symbols={symbols}
        activeSymbol={activeSymbol}
        onSymbolChange={setActiveSymbol}
        onHome={() => setScreen("landing")}
        onConsole={() => setScreen("console")}
      />
    );
  }

  if (!feed) {
    if (adapterKind === "websocket") {
      return (
        <div className="app">
          <header className="topbar">
            <div className="brand">
              <span className="dot" /> MoleOption Console
              <span className="badge">wave-12 live</span>
            </div>
          </header>
          <div className="empty-state">
            <h2>Live feed offline</h2>
            <p>
              The websocket adapter is a wave-12 placeholder.
              Run the mock feed by removing <code>?feed=live</code> from
              the URL.
            </p>
            <p>Status: {status}</p>
          </div>
        </div>
      );
    }
    return (
      <div className="app">
        <header className="topbar">
          <div className="brand">
            <span className="dot" /> MoleOption Console
            <span className="badge">connecting…</span>
          </div>
        </header>
        <div className="empty-state">Booting feed adapter…</div>
      </div>
    );
  }

  let panel: JSX.Element;
  switch (active) {
    case "overview":
      panel = <OverviewPanel feed={feed} prober={proberSnapshot} />;
      break;
    case "trader":
      panel = <TraderPanel feed={feed} wallet={wallet} />;
      break;
    case "indexer":
      panel = <IndexerPanel feed={feed} />;
      break;
    case "keeper":
      panel = <KeeperPanel feed={feed} wallet={wallet} />;
      break;
  }

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <button
            type="button"
            className="brand-home"
            onClick={() => setScreen("landing")}
          >
            <span className="dot" /> MoleOption Console
          </button>
          <button
            type="button"
            className="wallet-btn"
            onClick={() => setScreen("trade")}
          >
            ↗ 交易终端
          </button>
          <span className="badge">
            {adapterKind === "mock" ? "wave-12 mock" : "wave-12 live"} ·{" "}
            {status}
          </span>
        </div>
        <div className="topbar-meta">
          <span>slot {formatSlot(feed.indexer.slot)}</span>
          <span>{feed.indexer.market.symbol}</span>
          <span>schema v{feed.indexer.market.schemaVersion}</span>
          <span className={`status status-${feed.keeper.status}`}>
            keeper: {feed.keeper.status}
          </span>
          <span className={`status wallet-${walletStatus}`}>
            wallet: {walletStatus}
            {walletPubkey ? ` · ${formatPubkey(walletPubkey.hex)}` : ""}
          </span>
          {walletStatus === "connected" ? (
            <button
              type="button"
              className="wallet-btn"
              onClick={() => {
                void onWalletDisconnect();
              }}
            >
              disconnect
            </button>
          ) : (
            <button
              type="button"
              className="wallet-btn primary"
              onClick={() => {
                void onWalletConnect();
              }}
              disabled={walletStatus === "connecting"}
            >
              connect mock wallet
            </button>
          )}
        </div>
      </header>
      {feed.marketsView && symbols.length > 0 ? (
        // Wave 19 — multi-market selector. Renders one pill per
        // configured market with a freshness dot. Selection is
        // persisted in URL + localStorage by `useActiveMarket`.
        <MarketSelector
          symbols={symbols}
          active={activeSymbol}
          onChange={setActiveSymbol}
          view={feed.marketsView}
          positions={feed.positions}
          {...(feed.currentSlot !== undefined && {
            currentSlot: feed.currentSlot,
          })}
        />
      ) : null}
      <nav className="tabs">
        {TABS.map((t) => (
          <button
            key={t.id}
            className={`tab ${active === t.id ? "active" : ""}`}
            onClick={() => setActive(t.id)}
            type="button"
          >
            <span className="tab-label">{t.label}</span>
            <span className="tab-desc">{t.description}</span>
          </button>
        ))}
      </nav>
      {/*
       * Wave 16/17 — render the keeper-leader-lock banner ABOVE every
       * panel so multi-replica deployments can confirm leadership at
       * a glance from any tab. Wave 17 wired this to live data:
       *   • the websocket adapter subscribes to the lock PDA when
       *     `VITE_MARKET_PDA` is set,
       *   • the bytes flow through `feed.keeperLeaderLockBytes`,
       *   • we decode here via wasm and feed the typed view.
       * For the mock adapter (`?feed=mock`) `feed.keeperLeaderLockBytes`
       * stays undefined and the banner shows the truthful
       * `uninitialised` state; production wiring closes the loop.
       *
       * `currentSlot` prefers the live cluster reading from the
       * adapter (`feed.currentSlot`) and falls back to the indexer
       * tick slot when the adapter doesn't track the cluster clock.
       */}
      {feed.marketsView ? (
        // Wave 18 — multi-market grid. Replaces the wave-16 banner
        // when the operator opted into `VITE_MARKETS`. The grid
        // renders one row per configured market and flags
        // `expected_leader` mismatches inline.
        <LeaderLockGrid
          view={feed.marketsView}
          currentSlot={feed.currentSlot ?? BigInt(feed.indexer.slot)}
          decode={decodeKeeperLeaderLockBytes}
          {...(expectedLeaders ? { expected: expectedLeaders } : {})}
        />
      ) : (
        <LeaderLockBanner
          view={leaderLockView}
          currentSlot={feed.currentSlot ?? BigInt(feed.indexer.slot)}
        />
      )}
      {/*
       * Wave 26 — prober health snapshot. Renders only when
       * `VITE_PROBER_SNAPSHOT_URL` points at the JSON the
       * `ops-toolkit prober` daemon publishes each cycle, surfacing the
       * per-market `position_principal_drift` verdict (now fed by live
       * open-interest) alongside any other firing checks.
       */}
      <ProberHealthPanel snapshot={proberSnapshot} />
      <main className="panel-host">{panel}</main>
      <footer className="footer">
        <span>
          Wave 12 · adapter ={" "}
          <code>
            {adapterKind} ({status})
          </code>{" "}
          · wallet = <code>{wallet.name}</code> · use <code>?feed=live</code>{" "}
          to preview the websocket adapter (wave-13 wires real RPC)
        </span>
      </footer>
    </div>
  );
}
