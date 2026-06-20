import { useEffect, useMemo, useState } from "react";
import type { JSX } from "react";
import { Routes, Route, Navigate, useNavigate } from "react-router-dom";

import { LandingPage } from "./landing/LandingPage";
import { TradeView } from "./trade/TradeView";
import { DashboardScreen } from "./screens/DashboardScreen";
import { AdminScreen } from "./screens/AdminScreen";
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

interface AdapterBuild {
  adapter: FeedAdapter;
  expectedLeaders?: Map<string, string>;
}

function buildAdapter(): AdapterBuild {
  const kind = adapterKindFromUrl();
  if (kind === "websocket") {
    const env = (import.meta as unknown as {
      env?: Record<string, string | undefined>;
    }).env;
    const rpcUrl = env?.VITE_RPC_URL;
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
  const navigate = useNavigate();
  const built = useMemo(() => buildAdapter(), []);
  const adapter = built.adapter;
  const expectedLeaders = built.expectedLeaders;

  const wallet = useMemo<WalletAdapter>(() => selectWalletAdapter(), []);
  const [walletStatus, setWalletStatus] = useState<WalletStatus>(wallet.status());
  const [walletPubkey, setWalletPubkey] = useState(wallet.pubkey());

  const { snapshot: rawFeed, status, adapterKind } = useFeed(adapter);
  const keeperByMarket = useKeeperMetricsMulti();
  const proberSnapshot = useProberSnapshot();

  const feedWithKeeper = useMemo(() => {
    if (!rawFeed || !keeperByMarket || keeperByMarket.size === 0) {
      return rawFeed;
    }
    return mergeKeeperMetricsIntoFeed(rawFeed, keeperByMarket);
  }, [rawFeed, keeperByMarket]);

  const symbols = useMemo<string[]>(() => {
    if (!feedWithKeeper?.marketsView) return [];
    return Array.from(feedWithKeeper.marketsView.entries.keys()).sort();
  }, [feedWithKeeper?.marketsView]);
  const [activeSymbol, setActiveSymbol] = useActiveMarket(symbols);

  const feed = useMemo(() => {
    if (!feedWithKeeper) return feedWithKeeper;
    return selectActiveMarketSnapshot(feedWithKeeper, activeSymbol);
  }, [feedWithKeeper, activeSymbol]);

  useEffect(() => {
    const id = setInterval(() => {
      setWalletStatus(wallet.status());
      setWalletPubkey(wallet.pubkey());
    }, 250);
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

  const walletProps = {
    wallet,
    walletStatus,
    ...(walletPubkey?.hex ? { walletPubkeyHex: walletPubkey.hex } : {}),
    onConnect: () => void onWalletConnect(),
    onDisconnect: () => void onWalletDisconnect(),
  };

  const marketProps = {
    symbols,
    activeSymbol,
    onSymbolChange: setActiveSymbol,
  };

  return (
    <Routes>
      <Route
        path="/"
        element={
          <LandingPage
            onLaunch={() => navigate("/app")}
            onConsole={() => navigate("/dashboard")}
            livePriceUsd={livePriceUsd}
            liveSymbol={feed?.indexer.market.symbol ?? "SOL-PERP"}
          />
        }
      />
      <Route
        path="/app"
        element={
          feed ? (
            <TradeView
              feed={feed}
              {...walletProps}
              {...marketProps}
              onHome={() => navigate("/")}
              onConsole={() => navigate("/dashboard")}
            />
          ) : (
            <BootScreen status={status} onHome={() => navigate("/")} />
          )
        }
      />
      <Route
        path="/dashboard"
        element={
          <DashboardScreen
            feed={feed}
            prober={proberSnapshot}
            adapterKind={adapterKind}
            status={status}
            {...walletProps}
            {...marketProps}
            onHome={() => navigate("/")}
            onTrade={() => navigate("/app")}
          />
        }
      />
      <Route
        path="/admin"
        element={
          <AdminScreen
            feed={feed}
            status={status}
            {...(expectedLeaders ? { expectedLeaders } : {})}
            {...walletProps}
            {...marketProps}
            onHome={() => navigate("/")}
          />
        }
      />
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}

function BootScreen({
  status,
  onHome,
}: {
  status: string;
  onHome: () => void;
}): JSX.Element {
  return (
    <div className="app">
      <div className="empty-state">
        <h2>Connecting to feed…</h2>
        <p>Waiting for the RPC feed to start (status: {status}).</p>
        <button type="button" className="wallet-btn" onClick={onHome}>
          Back to home
        </button>
      </div>
    </div>
  );
}
