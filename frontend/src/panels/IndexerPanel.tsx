import type { JSX } from "react";
import type { FeedSnapshot } from "../types";
import {
  formatBigQty,
  formatPriceMicro,
  formatPubkey,
  formatSlot,
  formatUsdcMicro,
} from "../format";

interface Props {
  feed: FeedSnapshot;
}

export function IndexerPanel({ feed }: Props): JSX.Element {
  const ix = feed.indexer;
  const totalLong = ix.subPools.reduce((acc, s) => acc + s.totalOpenLongQty, 0n);
  const totalShort = ix.subPools.reduce((acc, s) => acc + s.totalOpenShortQty, 0n);
  const totalDormant = ix.subPools.reduce(
    (acc, s) => acc + s.dormantInventory.Long + s.dormantInventory.Short,
    0,
  );

  return (
    <div className="panel indexer-panel">
      <section className="stat-strip">
        <Stat label="Cluster slot" value={formatSlot(ix.slot)} />
        <Stat label="Mid price (USDC)" value={`$${formatPriceMicro(ix.market.midPriceMicro)}`} />
        <Stat label="Total open Long" value={formatBigQty(totalLong)} />
        <Stat label="Total open Short" value={formatBigQty(totalShort)} />
        <Stat label="Dormant ticks (sum)" value={String(totalDormant)} />
        <Stat
          label="Recovery outstanding"
          value={`$${formatUsdcMicro(ix.projectedRecoveryOutstandingMicroUsdc)}`}
          warn={ix.projectedRecoveryOutstandingMicroUsdc > 100_000_000_000n}
        />
      </section>

      <section className="card">
        <h2>Sub-pools ({ix.subPools.length})</h2>
        <table className="data-table">
          <thead>
            <tr>
              <th>ID</th>
              <th>PDA</th>
              <th>Open Long</th>
              <th>Open Short</th>
              <th>Long collateral</th>
              <th>Short collateral</th>
              <th>Dormant L</th>
              <th>Dormant S</th>
            </tr>
          </thead>
          <tbody>
            {ix.subPools.map((s) => (
              <tr key={s.id}>
                <td>#{s.id}</td>
                <td className="mono">{formatPubkey(s.pubkey.hex)}</td>
                <td>{formatBigQty(s.totalOpenLongQty)}</td>
                <td>{formatBigQty(s.totalOpenShortQty)}</td>
                <td>${formatUsdcMicro(s.longCollateral)}</td>
                <td>${formatUsdcMicro(s.shortCollateral)}</td>
                <td>{s.dormantInventory.Long}</td>
                <td>{s.dormantInventory.Short}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>

      <section className="card">
        <h2>Dormant buckets ({ix.dormantBuckets.length})</h2>
        <table className="data-table">
          <thead>
            <tr>
              <th>Sub-pool</th>
              <th>Direction</th>
              <th>Tick</th>
              <th>Total shares</th>
              <th>Pending recovery</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {ix.dormantBuckets.slice(0, 16).map((b, i) => (
              <tr key={`${b.subPoolId}-${b.direction}-${b.tick}-${i}`}>
                <td>#{b.subPoolId}</td>
                <td className={`side-${b.direction.toLowerCase()}`}>{b.direction}</td>
                <td>{b.tick}</td>
                <td>{formatBigQty(b.totalShares)}</td>
                <td>${formatUsdcMicro(b.pendingRecoveryMicroUsdc)}</td>
                <td>
                  {b.readyToClose ? (
                    <span className="status status-running">close-ready</span>
                  ) : (
                    <span className="status status-warming_up">live</span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>

      <section className="card">
        <h2>Pending init hints ({ix.pendingInitHints.length})</h2>
        {ix.pendingInitHints.length === 0 ? (
          <p className="empty">No queued init hints — keeper is keeping up.</p>
        ) : (
          <table className="data-table">
            <thead>
              <tr>
                <th>Sub-pool</th>
                <th>Direction</th>
                <th>Tick</th>
                <th>First seen at slot</th>
                <th>Slots aged</th>
              </tr>
            </thead>
            <tbody>
              {ix.pendingInitHints.map((h, i) => (
                <tr key={`${h.subPoolId}-${h.tick}-${i}`}>
                  <td>#{h.subPoolId}</td>
                  <td className={`side-${h.direction.toLowerCase()}`}>{h.direction}</td>
                  <td>{h.tick}</td>
                  <td>{formatSlot(h.hintSlot)}</td>
                  <td>{ix.slot - h.hintSlot}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </div>
  );
}

function Stat({
  label,
  value,
  warn = false,
}: {
  label: string;
  value: string;
  warn?: boolean;
}): JSX.Element {
  return (
    <div className={`stat ${warn ? "stat-warn" : ""}`}>
      <span className="stat-label">{label}</span>
      <span className="stat-value">{value}</span>
    </div>
  );
}
