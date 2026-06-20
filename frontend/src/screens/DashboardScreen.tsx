import type { JSX } from "react";
import { useTranslation } from "react-i18next";

import type { FeedSnapshot } from "../types";
import type { WalletAdapter } from "../wallet";
import type { ProberSnapshot } from "../feed/proberSnapshot";
import { OverviewPanel } from "../panels/OverviewPanel";
import { MarketSelector } from "../panels/MarketSelector";
import { ProberHealthPanel } from "../panels/ProberHealthPanel";
import { AppHeader } from "./AppHeader";
import "./screens.css";

interface Props {
  feed: FeedSnapshot | null | undefined;
  prober: ProberSnapshot | null;
  adapterKind: string;
  status: string;
  wallet: WalletAdapter;
  walletStatus: string;
  walletPubkeyHex?: string;
  onConnect: () => void;
  onDisconnect: () => void;
  symbols: string[];
  activeSymbol: string | null;
  onSymbolChange: (s: string) => void;
  onHome: () => void;
  onTrade: () => void;
}

export function DashboardScreen({
  feed,
  prober,
  status,
  walletStatus,
  walletPubkeyHex,
  onConnect,
  onDisconnect,
  symbols,
  activeSymbol,
  onSymbolChange,
  onHome,
  onTrade,
}: Props): JSX.Element {
  const { t } = useTranslation();

  return (
    <div className="sc-page">
      <AppHeader
        variant="dashboard"
        walletStatus={walletStatus}
        {...(walletPubkeyHex ? { walletPubkeyHex } : {})}
        onConnect={onConnect}
        onDisconnect={onDisconnect}
        onHome={onHome}
        onTrade={onTrade}
      />

      <main className="sc-main">
        <div className="sc-title-row">
          <div>
            <h1 className="sc-title">{t("dashboard.title")}</h1>
            <p className="sc-subtitle">{t("dashboard.subtitle")}</p>
          </div>
        </div>

        {!feed ? (
          <div className="sc-loading">{status}…</div>
        ) : (
          <>
            {feed.marketsView && symbols.length > 0 ? (
              <MarketSelector
                symbols={symbols}
                active={activeSymbol ?? symbols[0] ?? ""}
                onChange={onSymbolChange}
                view={feed.marketsView}
                positions={feed.positions}
                {...(feed.currentSlot !== undefined && {
                  currentSlot: feed.currentSlot,
                })}
              />
            ) : null}
            <ProberHealthPanel snapshot={prober} />
            <OverviewPanel feed={feed} prober={prober} />
          </>
        )}
      </main>
    </div>
  );
}
