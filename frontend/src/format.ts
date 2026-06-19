// Wave 11 formatting helpers shared across panels.
//
// Centralised so number formatting matches across panels (the
// keeper console and indexer dashboard both show the same prices,
// it'd be confusing if one rounded differently).

export function formatPriceMicro(microUsdc: bigint): string {
  const whole = microUsdc / 1_000_000n;
  const frac = microUsdc % 1_000_000n;
  return `${whole.toString()}.${frac.toString().padStart(6, "0")}`;
}

export function formatUsdcMicro(microUsdc: bigint): string {
  const sign = microUsdc < 0n ? "-" : "";
  const abs = microUsdc < 0n ? -microUsdc : microUsdc;
  const whole = abs / 1_000_000n;
  const frac = abs % 1_000_000n;
  // Group thousands.
  const wholeStr = whole.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return `${sign}${wholeStr}.${frac.toString().padStart(6, "0")}`;
}

export function formatPubkey(hex: string): string {
  if (hex.length <= 16) return hex;
  return `${hex.slice(0, 6)}…${hex.slice(-6)}`;
}

export function formatSlot(slot: number): string {
  return slot.toLocaleString("en-US");
}

export function formatPercent(pct: number, decimals = 2): string {
  return `${(pct * 100).toFixed(decimals)}%`;
}

export function formatVol(vol: number | null): string {
  if (vol === null) return "—";
  return vol.toFixed(3);
}

export function formatBigQty(qty: bigint): string {
  return qty.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}
