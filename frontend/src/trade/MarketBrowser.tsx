import { useMemo, useState } from "react";
import type { JSX } from "react";
import { useTranslation } from "react-i18next";

import {
  CATALOG,
  CATEGORIES,
  findSymbol,
  formatQuote,
  type MarketCategory,
} from "../markets/catalog";
import type { Ticker } from "../markets/syntheticTicker";

interface Props {
  /** Currently selected underlying base. */
  activeBase: string;
  tickers: Map<string, Ticker>;
  onSelect: (base: string) => void;
  onClose: () => void;
}

function compactUsd(v: number): string {
  if (v >= 1_000_000_000) return `$${(v / 1_000_000_000).toFixed(2)}B`;
  if (v >= 1_000_000) return `$${(v / 1_000_000).toFixed(1)}M`;
  if (v >= 1_000) return `$${(v / 1_000).toFixed(0)}K`;
  return `$${v.toFixed(0)}`;
}

type Tab = "All" | MarketCategory;

/**
 * Hyperliquid-style market picker: a category-tabbed, searchable table
 * of every catalog underlying with live(ish) price, 24h change, volume,
 * and the max leverage tier available for it.
 */
export function MarketBrowser({
  activeBase,
  tickers,
  onSelect,
  onClose,
}: Props): JSX.Element {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("All");
  const [query, setQuery] = useState("");

  const rows = useMemo(() => {
    const q = query.trim().toUpperCase();
    return CATALOG.filter((s) => {
      if (tab !== "All" && s.category !== tab) return false;
      if (q && !s.base.toUpperCase().includes(q) && !s.name.toUpperCase().includes(q)) {
        return false;
      }
      return true;
    }).sort(
      (a, b) =>
        (tickers.get(b.base)?.volume24hUsd ?? 0) -
        (tickers.get(a.base)?.volume24hUsd ?? 0),
    );
  }, [tab, query, tickers]);

  const tabs: Tab[] = ["All", ...CATEGORIES];

  return (
    <>
      <div className="mb-backdrop" onClick={onClose} />
      <div className="mb-panel" role="dialog" aria-label={t("market.selectTitle")}>
        <div className="mb-search">
          <input
            autoFocus
            type="text"
            placeholder={t("market.searchPlaceholder")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <button type="button" className="mb-close" onClick={onClose} aria-label="close">
            ✕
          </button>
        </div>

        <div className="mb-tabs">
          {tabs.map((tb) => (
            <button
              key={tb}
              type="button"
              className={tab === tb ? "active" : ""}
              onClick={() => setTab(tb)}
            >
              {tb === "All" ? t("market.tabAll") : tb}
            </button>
          ))}
        </div>

        <div className="mb-table-head">
          <span className="mb-col-sym">{t("market.colSymbol")}</span>
          <span className="mb-col-num">{t("market.colPrice")}</span>
          <span className="mb-col-num">{t("market.col24h")}</span>
          <span className="mb-col-num">{t("market.colVolume")}</span>
        </div>

        <div className="mb-rows">
          {rows.length === 0 ? (
            <div className="mb-empty">{t("market.noResults")}</div>
          ) : (
            rows.map((s) => {
              const tk = tickers.get(s.base);
              const change = tk?.change24hPct ?? 0;
              const up = change >= 0;
              const active = s.base === activeBase;
              return (
                <button
                  key={s.base}
                  type="button"
                  className={`mb-row ${active ? "active" : ""}`}
                  onClick={() => {
                    onSelect(s.base);
                    onClose();
                  }}
                >
                  <span className="mb-col-sym">
                    <span className="mb-sym-base">{s.base}-USDC</span>
                    <span className="mb-sym-lev">{s.maxLeverage}x</span>
                    <span className="mb-sym-name">{s.name}</span>
                  </span>
                  <span className="mb-col-num mb-mono">
                    {tk ? formatQuote(tk.price, s) : "—"}
                    {tk?.live ? <em className="mb-live" title="live oracle">●</em> : null}
                  </span>
                  <span className={`mb-col-num mb-mono ${up ? "pos" : "neg"}`}>
                    {up ? "+" : ""}
                    {change.toFixed(2)}%
                  </span>
                  <span className="mb-col-num mb-mono mb-dim">
                    {tk ? compactUsd(tk.volume24hUsd) : "—"}
                  </span>
                </button>
              );
            })
          )}
        </div>

        <div className="mb-foot">
          {t("market.footNote", {
            count: CATALOG.length,
            crypto: CATALOG.filter((s) => s.assetClass === "crypto").length,
            tradfi: CATALOG.filter((s) => s.assetClass === "equity").length,
            fx: CATALOG.filter((s) => s.assetClass === "fx").length,
          })}
        </div>
      </div>
    </>
  );
}

/** Convenience: resolve a base into its display label or fall back. */
export function symbolLabel(base: string): string {
  return findSymbol(base)?.name ?? base;
}
