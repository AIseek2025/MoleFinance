// Market catalog — typed accessors over `catalog.json` (the single
// source of truth shared with the devnet bootstrap script).
//
// Domain model (see Docs/Planning/08-杠杆交易场与风控设计.md): every
// leverage tier is an INDEPENDENT on-chain Market. The on-chain symbol
// encodes both the underlying and the tier as `${base}-${lev}X`
// (e.g. "BTC-100X"), which always fits Market.symbol ([u8;16]).
//
// Per asset-class leverage caps:
//   crypto                       → 100x
//   equity / metals / commodities → 200x
//   fx (forex)                   → 500x

import raw from "./catalog.json";

export type AssetClass = "crypto" | "equity" | "fx";
export type MarketCategory = "Crypto" | "Tradfi" | "FX";

export interface CatalogSymbol {
  /** Underlying base ticker, e.g. "BTC". */
  base: string;
  /** Human-readable name. */
  name: string;
  assetClass: AssetClass;
  /** Highest leverage tier available for this underlying. */
  maxLeverage: number;
  /** Reference USD price used to seed synthetic feeds + chart history. */
  basePriceUsd: number;
  /** Display decimals for the quote price. */
  quoteDecimals: number;
  /** UI grouping tab. */
  category: MarketCategory;
}

interface RawSymbol {
  base: string;
  name: string;
  class: AssetClass;
  basePriceUsd: number;
  quoteDecimals: number;
}

interface RawClass {
  maxLeverage: number;
  category: MarketCategory;
}

const ASSET_CLASSES = raw.assetClasses as Record<AssetClass, RawClass>;

/** All available leverage tiers, ascending. */
export const LEVERAGE_TIERS: readonly number[] = raw.leverageTiers;

/** Tab order for the market browser. */
export const CATEGORIES: readonly MarketCategory[] = ["Crypto", "Tradfi", "FX"];

/** Full catalog, leverage cap + category resolved from the asset class. */
export const CATALOG: readonly CatalogSymbol[] = (raw.symbols as RawSymbol[]).map(
  (s) => {
    const cls = ASSET_CLASSES[s.class];
    return {
      base: s.base,
      name: s.name,
      assetClass: s.class,
      maxLeverage: cls.maxLeverage,
      basePriceUsd: s.basePriceUsd,
      quoteDecimals: s.quoteDecimals,
      category: cls.category,
    };
  },
);

const BY_BASE = new Map<string, CatalogSymbol>(CATALOG.map((s) => [s.base, s]));

/** Look up a catalog entry by its base ticker. */
export function findSymbol(base: string): CatalogSymbol | undefined {
  return BY_BASE.get(base);
}

/** Leverage tiers available for an underlying (filtered by its class cap). */
export function tiersFor(sym: Pick<CatalogSymbol, "maxLeverage">): number[] {
  return LEVERAGE_TIERS.filter((t) => t <= sym.maxLeverage);
}

/** On-chain market symbol for an (underlying, leverage) pair. */
export function marketSymbol(base: string, leverage: number): string {
  return `${base}-${leverage}X`;
}

/** Parse a `${base}-${lev}X` market symbol back into its parts. */
export function parseMarketSymbol(
  symbol: string,
): { base: string; leverage: number } | null {
  const m = /^(.+)-(\d+)X$/.exec(symbol);
  if (!m) return null;
  return { base: m[1]!, leverage: Number(m[2]) };
}

/**
 * Extract the underlying base from any market label the system might
 * carry — `${base}-${lev}X`, the legacy `${base}-PERP`, or a bare base.
 * Used to match a live on-chain feed price to a catalog underlying.
 */
export function baseOf(symbol: string): string {
  const tiered = parseMarketSymbol(symbol);
  if (tiered) return tiered.base;
  const perp = /^(.+)-PERP$/.exec(symbol);
  if (perp) return perp[1]!;
  return symbol;
}

/** Format a quote price with the underlying's configured precision. */
export function formatQuote(price: number, sym: CatalogSymbol): string {
  return price.toLocaleString("en-US", {
    minimumFractionDigits: sym.quoteDecimals,
    maximumFractionDigits: sym.quoteDecimals,
  });
}
