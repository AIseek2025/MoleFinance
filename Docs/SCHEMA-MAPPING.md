# Schema Mapping — Rust ↔ TypeScript

> **Status**: Wave 13 — initial publication.
> **Purpose**: explicit, line-for-line accountability of what each
> `Onchain<T>` Borsh field maps to in the frontend `FeedSnapshot`
> tree. Every field must have an entry; CI (`scripts/
> verify-schema-mapping.sh`) blocks PRs that add a Rust field without
> declaring its TypeScript fate.

The frontend never displays the raw on-chain account; it consumes a
**projection** (`FeedSnapshot` defined in `frontend/src/types.ts`)
that:

1. Drops protocol-internal accounting (`*_dust`, `*_pad`, `bump`,
   `*_active_generation`, …) — these matter to the engine but don't
   help an end user understand their position.
2. Renames to camelCase + a unit-bearing suffix (e.g.
   `long_pool_equity` → `longCollateral` after we lift collateral
   into a unit-aware bigint).
3. Aggregates per-direction sums into a single field where the UI
   doesn't care to disaggregate (e.g. `dormantInventory: { Long, Short }`).

This mapping must be kept honest. If you add a Rust field, add a row
here too. If you remove a Rust field, delete its row. If you change a
Rust field's semantics, update the rationale.

---

## How CI verifies this file

`scripts/verify-schema-parity.sh` parses every `pub <name>: <ty>` line
from the `Onchain*` structs in **`crates/keeper-decoder/src/lib.rs`**
(wave 14 onwards — wave 13 used `crates/keeper-rpc/src/accounts.rs`,
which is now a thin re-export shim), strips trailing `,`, and asserts
each `<name>` appears as a **word-boundary** match somewhere in this
file. The check is purely existential — a developer who renames a
field is forced to update this doc, but the doc itself is the source
of truth for what the rename means.

The wave-14 crate split (`keeper-decoder` is the schema crate;
`keeper-rpc` is the host-side RPC adapter that re-exports from it)
exists so the same `Onchain*` definitions can compile to
`wasm32-unknown-unknown` and ship to the frontend via
`wasm-pack` (wave 15) without a parallel TypeScript reimplementation.
Frontend's wave-14 hand-rolled `@coral-xyz/borsh` schemas are a
**stopgap** — they are pinned by `crates/keeper-decoder::tests::
schema_descriptor_json_lists_every_struct_and_field` and the
matching frontend `decoder.test.ts` so a Rust-side schema bump that
forgets to update the TS will fail in CI.

Fields are scoped per struct via the section headers below; the
verifier doesn't enforce scoping (a field that exists in multiple
structs only needs one mention), but reviewers must keep them split
manually.

---

## OnchainSubPool (`crates/keeper-rpc/src/accounts.rs::OnchainSubPool`)

Mirrors `programs/mole-option/src/state.rs::SubPool`. One per
`(market, sub_pool_id)`.

| Rust field | TS surface | Rationale |
|------------|-----------|-----------|
| `market` | `subPools[i].pubkey` (the SubPool PDA, not the market PDA) — frontend reaches the parent market via `IndexerSnapshot.market.pubkey`. | Identifying which market this sub-pool belongs to is implicit in the snapshot tree. |
| `sub_pool_id` | `subPools[i].id` | Direct rename to TS conventional camelCase. |
| `long_pool_equity` | `subPools[i].longCollateral` | Renamed to user-facing terminology ("collateral" rather than the accounting term "pool equity"). |
| `short_pool_equity` | `subPools[i].shortCollateral` | Same. |
| `long_active_shares` | omitted (engine internal) | Active shares are an O(1) accounting trick; users don't need to see them. Indexer recovers per-position locked-loss separately. |
| `short_active_shares` | omitted (engine internal) | Same. |
| `long_recovery_shares` | omitted (engine internal) | Recovery shares are reflected in `dormantBuckets[*].totalShares` after rotation. |
| `short_recovery_shares` | omitted (engine internal) | Same. |
| `long_active_notional` | aggregated into `subPools[i].totalOpenLongQty` | Notional is derived from active_shares × pool_equity in the indexer; frontend pulls the post-aggregation `totalOpenLongQty` instead. |
| `short_active_notional` | aggregated into `subPools[i].totalOpenShortQty` | Same. |
| `long_active_generation` | omitted (engine internal) | Generation counters are how the engine knows when an active→recovery rotation finished; not user-visible. |
| `short_active_generation` | omitted (engine internal) | Same. |
| `last_price` | folded into `IndexerSnapshot.market.midPriceMicro` (per-market mid; sub-pool last_price is the same value at sync time) | Per-sub-pool last_price is rarely needed at the UI level. |
| `last_sync_slot` | omitted (engine internal) | Used by keeper to compute σ̂ samples; doesn't add UI value. |
| `long_dust` | omitted (engine internal) | Floor residuals; fold into protocol fee accounting. |
| `short_dust` | omitted (engine internal) | Same. |
| `long_dormant_bucket_count` | derived into `subPools[i].dormantInventory.Long` | Same number, exposed under a more user-friendly name and a per-direction record. |
| `short_dormant_bucket_count` | derived into `subPools[i].dormantInventory.Short` | Same. |
| `bump` | omitted (anchor internal) | PDA bump seed; useful only for rebuilding the PDA, which the frontend never does. |
| `_pad` | omitted (alignment) | Borsh padding to keep the on-chain account a stable size; never user-relevant. |

## OnchainDormantBucket (`crates/keeper-rpc/src/accounts.rs::OnchainDormantBucket`)

Mirrors `programs/mole-option/src/state.rs::DormantBucket`.

| Rust field | TS surface | Rationale |
|------------|-----------|-----------|
| `sub_pool` | `dormantBuckets[i].subPoolId` (after PDA → id mapping in the indexer) | Frontend identifies sub-pools by id, not by Pubkey32. Indexer translates. |
| `direction_is_long` | `dormantBuckets[i].direction: "Long" \| "Short"` | Cleaner enum-as-string for TS. |
| `zero_price_tick` | `dormantBuckets[i].tick` | Direct rename ("tick" is the user-facing name in the planning docs). |
| `anchor_price` | omitted (engine internal) | The price at which the rotation occurred; useful for the engine's lazy migration path, not for users. |
| `total_recovery_shares` | `dormantBuckets[i].totalShares` | Renamed; this IS the user-relevant share count for a dormant bucket. |
| `total_recovery_notional` | folded into `dormantBuckets[i].pendingRecoveryMicroUsdc` | Notional → USDC outstanding is what the user wants to see. |
| `accrued_value` | folded into `dormantBuckets[i].pendingRecoveryMicroUsdc` | Same. |
| `position_count` | omitted (engine internal) | How many positions still hold this bucket; aggregate-only signal. |
| `last_applied_index` | drives `dormantBuckets[i].readyToClose` boolean | Frontend sees the binary "ready to close" flag derived from `last_applied_index == DistributionLedger.next_event_index`. |
| `bump` | omitted (anchor internal) | Same. |
| `_pad` | omitted (alignment) | Same. |

## OnchainDistEntry (`crates/keeper-rpc/src/accounts.rs::OnchainDistEntry`)

One row in a `DistributionLedger.entries` ring. Frontend never
materialises individual entries — the indexer collapses them into the
per-bucket recovery summary.

| Rust field | TS surface | Rationale |
|------------|-----------|-----------|
| `event_index` | omitted (engine internal) | Sequence number used by lazy migration replay. |
| `p_at_event` | omitted (engine internal) | Frontend doesn't display per-event price history; that's a wave-14 historical-chart feature. |
| `total_outstanding_at_event` | omitted (engine internal) | Same. |
| `total_alloc_input` | omitted (engine internal) | Same. |
| `allocated_sum_observed` | omitted (engine internal) | Same. |

## OnchainDistributionLedger (`crates/keeper-rpc/src/accounts.rs::OnchainDistributionLedger`)

| Rust field | TS surface | Rationale |
|------------|-----------|-----------|
| `sub_pool` | implicit in `pendingInitHints[i].subPoolId` (one hint per sub_pool × direction × tick) | Indexer maps Pubkey32 → numeric id at fetch time. |
| `direction_is_long` | `pendingInitHints[i].direction` | Boolean → enum string. |
| `max_entries` | omitted (engine internal) | Capacity bound; UI doesn't need to display it. |
| `gc_offset` | omitted (engine internal) | Garbage-collection cursor for the ring buffer. |
| `next_event_index` | drives `dormantBuckets[i].readyToClose` (compared against `last_applied_index`) | Same drives in `OnchainDormantBucket.last_applied_index`. |
| `accrued_value_total` | omitted (engine internal) | Aggregate is computed in indexer's `projectedRecoveryOutstandingMicroUsdc`. |
| `pending_distribution_total` | folded into `IndexerSnapshot.projectedRecoveryOutstandingMicroUsdc` | Cross-bucket sum. |
| `entry_count` | omitted (engine internal) | Frontend never iterates entries. |
| `entries` | omitted (engine internal) | Same; the entries themselves stay server-side. |
| `bump` | omitted (anchor internal) | Same. |
| `_pad` | omitted (alignment) | Same. |

## OnchainMarket (`crates/keeper-rpc/src/accounts.rs::OnchainMarket`)

Mirrors `programs/mole-option/src/state.rs::Market`. One per market.

| Rust field | TS surface | Rationale |
|------------|-----------|-----------|
| `global_config` | omitted (chain-level) | Indexer dereferences and surfaces `pausedGlobally` instead. |
| `symbol` | `market.symbol` | Bytes-to-UTF8 in indexer. |
| `collateral_mint` | omitted (chain-level) | UI shows the symbol; the underlying SPL mint isn't user-relevant once the symbol is established. |
| `vault` | omitted (chain-level) | Same. |
| `fee_vault` | omitted (chain-level) | Same. |
| `oracle_price_feed` | omitted (chain-level) | UI shows the resolved price, not the source PDA. |
| `oracle_program_id` | omitted (chain-level) | Same. |
| `leverage_bps` | exposed via wave-14 trade-form / not in `MarketSummary` yet | Wave 13 frontend treats leverage as a per-position concept (`PositionSummary`). |
| `min_margin` | exposed via wave-14 trade-form | Same. |
| `max_margin_per_position` | exposed via wave-14 trade-form | Same. |
| `max_total_principal` | omitted (engine internal) | Risk-limit field; UI may surface as a "market full" banner in wave 14. |
| `max_total_notional` | omitted (engine internal) | Same. |
| `current_total_principal` | omitted (engine internal) | Same. |
| `current_total_notional` | omitted (engine internal) | Same. |
| `open_fee_bps` | exposed via wave-14 trade-form | Same. |
| `max_oracle_age_seconds` | omitted (engine internal) | Used for `OracleStale` gate; UI shows derived "oracle lag" via `lastOracleSlot - currentSlot`. |
| `max_oracle_age_slots` | omitted (engine internal) | Same. |
| `max_confidence_bps` | omitted (engine internal) | Pyth confidence threshold; UI shows "ok / stale" in wave 14. |
| `max_price_move_bps_per_sync` | omitted (engine internal) | Anti-flash-crash circuit; not user-visible. |
| `price_tick` | omitted (engine internal) | Discretization grid for dormant buckets. |
| `tick_aggregation_factor` | omitted (engine internal) | Same. |
| `max_dormant_bucket_count_per_direction` | omitted (engine internal) | Engine capacity bound. |
| `dilution_safety_bps` | omitted (engine internal) | Pool-dilution rejection threshold (whitepaper §5.5). |
| `max_idle_slots` | omitted (engine internal) | Force-close timer for stale price feeds. |
| `paused` | `market.paused` | Direct. |
| `frozen_new_position` | `market.frozenNewPosition` | Direct. |
| `schema_version` | `market.schemaVersion` | Direct. |
| `sub_pool_count` | implicit in `subPools.length` | Indexer expands to actual pool array. |
| `dormant_distribute_mode` | omitted (engine internal) | Eager vs lazy routing flag. |
| `max_pending_apply_per_tx` | omitted (engine internal) | Per-tx batch limit. |
| `max_distribution_ledger_size` | omitted (engine internal) | DL ring capacity. |
| `bump` | omitted (anchor internal) | Same. |
| `_pad` | omitted (alignment) | Same. |

## OnchainPosition (`crates/keeper-decoder/src/lib.rs::OnchainPosition`) — Wave 21

Mirrors `programs/mole-option/src/state.rs::Position` byte-for-byte.
One per active trader position. Wave 21 lands the host-side decoder
so the wave-22 frontend `websocketAdapter` can `accountSubscribe` to
each position PDA and route it into the correct
`MarketViewEntry.positions` slot via the wave-20 `marketPdaHex`
field.

| Rust field | TS surface | Rationale |
|------------|-----------|-----------|
| `owner` | `PositionSummary.owner` | Direct. |
| `market` | `PositionSummary.marketPdaHex` | **Wave-20 multi-market routing key.** `selectActiveMarketSnapshot` filters by this. |
| `sub_pool` | `PositionSummary.subPoolId` (resolved via indexer) | Indexer maps PDA → `sub_pool_id` to keep the TS surface compact. |
| `position_id` | omitted (chain-level) | Internal id; users see the trade list ordered by `openedAt`. |
| `direction_is_long` | `PositionSummary.direction` (`"Long"`/`"Short"`) | Direct. |
| `status` | omitted (chain-level) | Closed / dormant positions are filtered server-side; TS only sees open positions. |
| `principal` | `PositionSummary.collateral` | Direct. |
| `leverage_bps` | omitted (engine internal) | Surfaced via the wave-14 trade form (per-market constant). |
| `notional` | derived (`principal × leverage_bps / 10_000`) | Indexer computes when needed. |
| `active_shares` | omitted (engine internal) | Pool-share book-keeping; UI shows derived PnL instead. |
| `recovery_shares` | omitted (engine internal) | Same — used to compute pending recovery. |
| `recovery_bucket_tick` | omitted (engine internal) | Linked indirectly via the dormant-bucket panel. |
| `has_recovery_bucket` | omitted (engine internal) | Drives indexer logic only. |
| `zero_price` | omitted (engine internal) | Locked-loss reference; surfaces as derived breakeven price in wave 22+. |
| `entry_price` | exposed in wave-22 trade history | Same. |
| `last_sync_slot` | omitted (chain-level) | Indexer aggregates per-market `last_sync_slot`. |
| `active_generation` | omitted (engine internal) | Pool rotation marker. |
| `qty` (synthesised) | `PositionSummary.qty` | Derived from `active_shares × current_pool_state` (already in TS). |
| `opened_at` | `PositionSummary.openedAt` | Direct (i64 → number, UNIX seconds). |
| `updated_at` | omitted (chain-level) | UI uses `openedAt`; refresh implied by feed snapshot age. |
| `closed_at` | omitted (chain-level) | Closed positions are filtered server-side. |
| `schema_version` | omitted (engine internal) | Migration housekeeping; UI doesn't surface. |
| `bump` | omitted (anchor internal) | Same. |
| `_pad` | omitted (alignment) | Same. |

---

## TypeScript surface ⊃ Rust mirror — what's _added_ on top

These TS fields are **synthesised** by the indexer from sequences of
on-chain events; they don't exist as a single Rust field but are
documented here so the inverse direction (TS → Rust) is also covered.

| TS field | Source | Notes |
|----------|--------|-------|
| `MarketSummary.pausedGlobally` | `GlobalConfig.paused_globally` | The frontend doesn't see `GlobalConfig` directly; indexer joins. |
| `MarketSummary.midPriceMicro` | `OnchainSubPool.last_price` (any sub-pool — they all share the same oracle) | Renamed to a unit-explicit name. |
| `MarketSummary.lastOracleSlot` | `OnchainSubPool.last_sync_slot` (likewise) | Renamed. |
| `MarketSummary.currentSlot` | RPC `getSlot` | Provided by the live feed adapter. |
| `SubPoolSummary.totalOpenLongQty` | derived from `long_active_notional / last_price` | The user-visible "open quantity" — wave 14 may switch to a more accurate per-position aggregation. |
| `SubPoolSummary.totalOpenShortQty` | derived from `short_active_notional / last_price` | Same. |
| `DormantBucketSummary.pendingRecoveryMicroUsdc` | `OnchainDormantBucket.{accrued_value, total_recovery_notional}` | Aggregated. |
| `DormantBucketSummary.readyToClose` | `OnchainDormantBucket.last_applied_index == OnchainDistributionLedger.next_event_index` | Boolean. |
| `IndexerSnapshot.projectedRecoveryOutstandingMicroUsdc` | sum of `pending_distribution_total` across all DLs | Cross-account. |
| `KeeperLoopMetrics.*` | from `keeper-bot::metrics` Prometheus exposition (parsed in indexer) | Wave-12 metrics surface. |
| `RotatePrediction.*` | from `keeper::RotateRiskPredictor` output stream | Wave-9 keeper. |
| `PositionSummary.*` | from indexer's per-position projection (wave 2) | Not on-chain mirror — indexer-replayed from events. |
