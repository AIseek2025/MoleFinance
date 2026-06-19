# Changelog

All notable changes to MoleOption are recorded here. We follow
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely and
group entries by **wave** rather than calendar release; each wave is
a self-contained delivery of "everything that needed to land before
moving to the next column on the project plan".

The detailed wave-by-wave engineering notes live in
`Docs/Planning/20-攻坚开发进度与里程碑.md`. This file is the
externally-facing summary: what changed, why it matters to operators
and integrators, and where to look for the proof.

---

## [Wave 28] — 2026-06-19 — pre-launch readiness review + protocol Overview landing page

Acting on a pre-launch development-planning request, we audited every
module against the PRD and roadmap (`Docs/Planning/25-上线前就绪评估.md`).
Verdict: the **on-chain instruction surface is launch-complete**
(init / sync / open / close / force-close / claim / pre-sync / harvest /
pause-resume-freeze / global-pause / schema migration / keeper-leader
lifecycle) — the remaining contract items are deploy-time actions (real
program ID, IDL publish) and external audit, all out-of-sandbox. The real
gap was a **protocol-level landing page**: every existing panel zooms into
ONE market, with no at-a-glance protocol pulse.

**Frontend — Overview landing page.**

- `feed/protocolStats.ts` — `aggregateProtocolStats(feed)` folds the live
  multi-market view into protocol headline numbers: total value locked
  (Σ sub-pool collateral), open positions, long/short collateral + net
  skew, recovery outstanding, and a per-market breakdown. Falls back to
  the lone `indexer` snapshot in single-market mode so the page is always
  meaningful. `longShareBps` drives the skew gauge. 8 vitest tests.
- `panels/OverviewPanel.tsx` — five KPI cards + a long/short skew bar +
  a per-market table (state pill, collateral, long/short, positions,
  recovery). Wired as a new `overview` tab, placed first and made the
  **default** landing tab.

**Backend — protocol rollup (mirror).**

- `ops_toolkit::protocol_summary` — `summarize_protocol(&MultiMarketHealthReport)`
  folds N per-market health reports into one `ProtocolSummary`
  (markets, healthy/warn/critical counts, total firing checks, worst exit
  code, overall status, highest firing severity) + a flat-JSON renderer
  for status pages / alert annotations. The backend twin of the frontend
  `protocolStats` aggregation. 4 unit tests.

**Verification.** `cargo test` 464/464 (wave-27 460 → +4) · clippy clean
(default + `solana-rpc`) · frontend vitest 152/152 (wave-27 144 → +8) ·
typecheck + build clean · all three governance verifiers green
(test-counts 464/464, schema-parity 103/103, security-references 32/32).

No contract changes (surface already launch-complete). `Docs/Planning/25`
records the full readiness matrix and the wave-29+ out-of-sandbox roadmap
(deploy-time program ID/IDL, devnet matrix, real wallet adapter, closed
PnL history, external audit).

---

## [Wave 27] — 2026-06-19 — live reported notional from the Market PDA

Wave 26 made the prober's *on-chain* side live (the open-interest
position sum), but the *reported* notional still came from a static
`RpcMarketFetcher` config constant. Wave 27 lifts it from the chain too:
straight off the program's own `Market.current_total_notional` counter.
The `position_principal_drift` check now reconciles two INDEPENDENT
on-chain truths — the program's running aggregate vs the sum of decoded
positions — a strictly stronger integrity signal than the wave-24
indexer-vs-positions reconciliation.

### Added

- **`RpcMarketFetcher` lifts the on-chain aggregate notional.** New
  `RpcMarketFetcherConfig.lift_reported_notional_from_market` (default
  `true`): when the `Market` PDA decodes, the fetcher copies
  `Market.current_total_notional` into
  `PoolFacts.total_notional_micro_usdc`, replacing the fixture default.
  A missing/garbled account leaves the indexer-supplied default intact;
  set the flag `false` to keep a dedicated scraper's figure. 4 fresh
  Rust tests (lift applied, flag-off preserved, missing-market
  preserved, default-on).
- **Frontend program-aggregate reconciliation.** `MarketSummary` gains
  optional `currentTotalPrincipal` / `currentTotalNotional` (populated
  by the real `onchainMarketToSummary` decode path; mock seeds a
  healthy demo value). New `reconcileProgramAggregate(positions,
  marketAggregatePrincipal)` reconciles the live position-collateral
  sum against the `Market.current_total_principal` counter (same
  thresholds as the wave-24 check), and `TraderPanel` renders a
  "Program aggregate" row alongside the existing "Indexer
  reconciliation". 4 fresh vitest cases.

### Verification

- `cargo test --workspace --all-targets` — **460 pass / 0 fail**
  (wave 26: 456). Net new: 4.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo clippy -p ops-toolkit --features solana-rpc --all-targets
  -- -D warnings` — clean.
- `npx vitest run` (frontend) — **144 pass / 0 fail** (wave 26: 140).
- `npx vitest run` (ops-toolkit/ts) — 33 pass / 0 fail.
- `bash scripts/verify-test-counts.sh` — declared 460, observed 460.
- `bash scripts/verify-schema-parity.sh` — 103 / 103.
- `bash scripts/verify-security-references.sh` — 32 / 32.
- Frontend `npm run build` + `tsc --noEmit` — clean.

### Wave 28 priorities

1. **solana-client `RpcAccountSource` shim** — give `RpcMarketFetcher`
   a real `get_multiple_accounts` / `get_slot` binding (behind
   `solana-rpc`) and wire the live base fetcher into the prober binary
   for an end-to-end real-cluster cycle (sandbox-external).
2. **devnet integration matrix** + **CI sandbox-external lanes** +
   **audit firm onboarding**.

---

## [Wave 26] — 2026-06-19 — prober daemon folds live open-interest + frontend health panel

Wave 25 made the `OpenInterestAugmentingFetcher` decorator and added a
feature-gated production constructor, but the actual `ops-toolkit prober`
daemon still ran the fixture-only base fetcher — so the
`position_principal_drift` check kept skipping in production. Wave 26
wires the decorator into the daemon and gives operators a frontend view
of the daemon's published verdict.

### Added

- **`ops-toolkit prober` folds live open-interest.** When built
  `--features solana-rpc` and `MOLE_OI_RPC_URL` is set, the prober
  daemon wraps its base `MarketFetcher` in
  `OpenInterestAugmentingFetcher::with_solana_rpc`, so every cycle
  scans live `Position` PDAs and reconciles per-market on-chain
  notional against the indexer figure — the drift check finally runs
  on real cluster data. Without the feature/env it falls back to the
  fixture context (drift check skips, no false alarm). The loop body
  was hoisted into a generic `drive_prober<F>` so the augmented and
  base paths share one implementation; `DemoFetcher` / `StdClock` /
  `FileSink` are now module-level for reuse.
- **Frontend prober-health panel.** New
  `feed/proberSnapshot.ts` parses the daemon's `render_json_multi`
  JSON snapshot (worst exit code + per-market overall status, counts,
  and decoded checks). `useProberSnapshot` polls
  `VITE_PROBER_SNAPSHOT_URL` (retains last-good on transient failure),
  and `ProberHealthPanel` renders each market's verdict with the
  `position_principal_drift` check surfaced first (status + drift %),
  plus any other firing checks. Renders nothing when the URL is unset,
  so mock / offline dev is unaffected.
- **Tests.** 7 fresh frontend tests (`proberSnapshot` parse, drift
  extraction, skipped-probe handling, firing-check ordering, malformed
  rejection).

### Verification

- `cargo test --workspace --all-targets` — **456 pass / 0 fail**.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo clippy -p ops-toolkit --features solana-rpc --all-targets
  -- -D warnings` — clean (compile-checks the prober's augmented path).
- `ops-toolkit prober <markets.toml> … 1 1` — default path writes a
  valid `render_json_multi` snapshot (drift check skips, exit 0).
- `npx vitest run` (frontend) — **140 pass / 0 fail** (wave 25: 133).
- `npx vitest run` (ops-toolkit/ts) — 33 pass / 0 fail.
- `bash scripts/verify-test-counts.sh` — declared 456, observed 456.
- `bash scripts/verify-schema-parity.sh` — 103 / 103.
- `bash scripts/verify-security-references.sh` — 32 / 32.
- Frontend `npm run build` + `tsc --noEmit` — clean.

### Wave 27 priorities

1. **Live RPC base `MarketFetcher`** — build the cluster-backed
   indexer→`HealthContext` fetcher so the prober's *reported* notional
   is also live (today the base ctx is still the fixture; only the
   on-chain open-interest side is live).
2. **devnet integration matrix** + **CI sandbox-external lanes** +
   **audit firm onboarding**.

---

## [Wave 25] — 2026-06-19 — open-interest goes live in the prober loop

Wave 24 added the `position_principal_drift` check but nothing fed it
real cluster data, so it always *skipped* (on-chain notional stuck at
0). Wave 25 closes the loop: the prober now scans live positions every
cycle and reconciles them **per market**, so the integrity signal
fires on production data instead of fixtures.

### Added

- **`OpenInterestAugmentingFetcher` (prober decorator).** New
  `ops_toolkit::prober::OpenInterestAugmentingFetcher<F, S>` wraps any
  base `MarketFetcher` plus an `AccountFetcher` source. On every
  `fetch` it runs a per-market open-interest scan and folds the result
  into `ctx.pool` via `apply_open_interest_to_pool`, so the wave-24
  `position_principal_drift` check reconciles real positions vs the
  indexer figure. The source is generic — `MockAccountFetcher` in
  tests, `SolanaRpcAccountFetcher` (behind `solana-rpc`) in
  production. Open-interest scan failures are **non-fatal**: the base
  context flows through unchanged (on-chain notional stays 0 ⇒ the
  drift check skips rather than false-alarms), so a transient
  `getProgramAccounts` hiccup never aborts the cycle.
- **Per-market open-interest scan.**
  `ops_toolkit::position_interest::fetch_open_interest_for_market` and
  `aggregate_open_interest_for_market` restrict a program-wide
  position scan to a single `market_pda`, so one RPC round-trip feeds
  every market's drift check in a multi-market cycle.
- **Frontend `reconcileByMarket`.** New
  `feed/openInterest::reconcileByMarket(positions, reportedByMarket)`
  groups the live feed by `marketPdaHex` and reconciles each market's
  on-chain collateral against the indexer-reported collateral —
  the frontend mirror of the backend's per-market check. The
  `MarketSelector` now shows each market's live position count as a
  pill badge and surfaces the reconciliation verdict + drift % in the
  pill tooltip.
- **`OpenInterestAugmentingFetcher::with_solana_rpc` (production
  constructor).** Feature-gated (`solana-rpc`) one-call builder that
  wraps any base `MarketFetcher` with a live `SolanaRpcAccountFetcher`
  open-interest source — the single entry point a production prober
  binary uses to fold real per-market open-interest into the drift
  check without re-assembling the `getProgramAccounts` plumbing.
  Compile-checked under `--features solana-rpc`; runtime is exercised
  on devnet.
- **Tests.** 5 fresh Rust tests (per-market aggregate/fetch filters,
  augmenting-fetcher folds OI into pool, scan-miss leaves on-chain 0,
  end-to-end drift check runs through the prober loop) + 2 frontend
  tests (`reconcileByMarket` per-market verdicts, single-sided
  markets).

### Verification

- `cargo test --workspace --all-targets` — **456 pass / 0 fail**
  (wave 24: 451). Net new: 5 (per-market open-interest +
  augmenting-fetcher).
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo clippy -p ops-toolkit --features solana-rpc --all-targets
  -- -D warnings` — clean (compile-checks
  `OpenInterestAugmentingFetcher::with_solana_rpc`).
- `npx vitest run` (frontend) — **133 pass / 0 fail** (wave 24:
  131). Net new: 2 `reconcileByMarket` tests.
- `npx vitest run` (ops-toolkit/ts) — 33 pass / 0 fail.
- `bash scripts/verify-test-counts.sh` — declared 456, observed
  456, slack 0.
- `bash scripts/verify-schema-parity.sh` — **103 / 103**.
- `bash scripts/verify-security-references.sh` — 32 / 32.
- Frontend `npm run build` + `tsc --noEmit` — clean.

### Wave 26 priorities

1. **Production prober daemon binary** — an `ops-toolkit prober`
   long-running mode that drives `ProberLoop` with a cluster-backed
   base `MarketFetcher` + `OpenInterestAugmentingFetcher::with_solana_rpc`,
   writing prom/json sinks to disk (needs devnet / self-hosted RPC).
2. **devnet integration matrix** + **CI sandbox-external lanes** +
   **audit firm onboarding**.

---

## [Wave 24] — 2026-05-24 — on-chain ↔ indexer principal reconciliation

Wave 23 shipped the open-interest aggregate on both ends. Wave 24
turns it into an integrity signal: a 22nd prober health check that
reconciles the indexer-reported pool notional against the independent
on-chain aggregate (sum of decoded `Position.notional`), plus a
frontend reconciliation badge on the open-interest card. A
non-trivial gap means the indexer and the chain disagree about live
exposure — a money-grade alert.

### Added

- **`position_principal_drift` health check (#22).** New
  `ops_toolkit::checks::check_position_principal_drift` compares
  `PoolFacts.onchain_position_notional_micro_usdc` (fed from
  `OpenInterestFacts::total_notional` via the new
  `apply_open_interest_to_pool` helper) against the indexer-reported
  `total_notional_micro_usdc`. Drift = `|onchain − reported| /
  max(reported, 1)`: Pass < 0.5 %, Warn (P2) < 2 %, Critical (P1)
  ≥ 2 %. Skips (Pass, `drift_enabled = 0`) when the open-interest
  probe didn't run this cycle — same "disabled-source ⇒ no false
  alarm" contract as the wave-17 leader-lock checks. The check
  battery grows 21 → 22. 6 fresh Rust tests (skip, reconciled,
  warn band, critical band, direction symmetry, `apply_*` helper).
- **`PoolFacts.onchain_position_notional_micro_usdc`.** New
  reconciliation input; defaults to `0` (probe-not-run). Existing
  fixtures updated; the demo / healthy contexts set it equal to the
  reported notional so the new check passes cleanly.
- **Frontend `reconcilePrincipal` + reconciliation badge.**
  `frontend/src/feed/openInterest.ts` gains `reconcilePrincipal(
  onchainCollateral, reportedCollateral)` returning an
  `ok / warn / critical / disabled` verdict with the same
  thresholds. `TraderPanel`'s open-interest card renders an "Indexer
  reconciliation" badge comparing the live-position collateral sum
  against the sub-pool reported collateral sum. 5 fresh vitest cases.

### Verification

- `cargo test --workspace --all-targets` — **451 pass / 0 fail**
  (wave 23: 445). Net new: 6 (`position_principal_drift` +
  `apply_open_interest_to_pool`).
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
  (default + `solana-rpc`).
- `npx vitest run` (frontend) — **131 pass / 0 fail** (wave 23:
  126). Net new: 5 `reconcilePrincipal` tests.
- `npx vitest run` (ops-toolkit/ts) — 33 pass / 0 fail.
- `bash scripts/verify-test-counts.sh` — declared 451, observed
  451, slack 0.
- `bash scripts/verify-schema-parity.sh` — **103 / 103**.
- `bash scripts/verify-security-references.sh` — 32 / 32.
- Frontend `npm run build` + `tsc --noEmit` — clean.

### Wave 25 priorities

1. **Wire `apply_open_interest_to_pool` into the live prober loop** —
   call `fetch_open_interest` per cycle (behind `solana-rpc`) so the
   drift check runs on real cluster data.
2. **devnet integration matrix** + **CI sandbox-external lanes** +
   **audit firm onboarding**.

---

## [Wave 23] — 2026-05-24 — open-interest aggregation from live positions

Wave 21 shipped the `OnchainPosition` decoder and named a future
"prober open-interest probe" that would decode every position via
`getProgramAccounts` and feed aggregate exposure. Wave 22 made
`feed.positions` a live, market-tagged stream. Wave 23 closes the
loop on **both** ends: a backend probe that scans + folds every
`Position` PDA into long/short exposure, and a frontend KPI card
that surfaces the same shape from the live feed.

### Added

- **`ops_toolkit::position_interest` — open-interest probe.**
  `aggregate_open_interest(&[OnchainPosition]) -> OpenInterestFacts`
  folds live positions into per-direction counts, principal, and
  notional totals (closed `status == 2` excluded, matching the
  frontend's `isDisplayablePosition`). `fetch_open_interest(fetcher,
  program_id)` runs a `getProgramAccounts` memcmp scan on the
  `Position` discriminator and decodes each account, counting
  decode failures without aborting. Written against the host-only
  `keeper_rpc::AccountFetcher` trait so the whole fetch→decode→
  aggregate pipeline is unit-tested with `MockAccountFetcher` (no
  cluster, no `solana-rpc` feature); production prober deployments
  pass a `SolanaRpcAccountFetcher` behind the feature. 6 fresh Rust
  tests (long/short split, closed exclusion, empty identity, short-
  heavy skew, decode-failure counting, program-account scan).
- **`frontend/src/feed/openInterest.ts` — open-interest KPI.**
  `aggregateOpenInterest(positions)` + `openInterestByMarket(positions)`
  mirror the backend shape (count / collateral / qty per direction).
  `TraderPanel` renders a "Market open interest" card driven by the
  now-live `feed.positions` (long/short collateral, qty, total, net
  skew). 5 fresh vitest cases.

### Verification

- `cargo test --workspace --all-targets` — **445 pass / 0 fail**
  (wave 22: 439). Net new: 6 `position_interest` tests.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo clippy -p ops-toolkit --features solana-rpc` — clean
  (compile-checks `fetch_open_interest` against
  `SolanaRpcAccountFetcher`).
- `npx vitest run` (frontend) — **126 pass / 0 fail** (wave 22:
  121). Net new: 5 `openInterest` tests.
- `npx vitest run` (ops-toolkit/ts) — 33 pass / 0 fail
  (unchanged).
- `bash scripts/verify-test-counts.sh` — declared 445, observed
  445, slack 0.
- `bash scripts/verify-schema-parity.sh` — **103 / 103**.
- `bash scripts/verify-security-references.sh` — 32 / 32.
- Frontend `npm run build` + `tsc --noEmit` — clean.

### Wave 24 priorities

1. **`MarketFacts.current_total_principal` reconciliation** — wire
   `OpenInterestFacts.total_principal` into a prober drift check
   against the indexer's reported principal.
2. **devnet integration matrix** — `ops-toolkit prober
   --features solana-rpc` against a 2-market devnet; capture
   `primary_backup_slot_diff` + open-interest distributions.
3. **CI sandbox-external lanes** + audit firm onboarding.

---

## [Wave 22] — 2026-05-24 — live position decode, `/metrics-multi` frontend merge, `serve-multi` daemon

Wave 21 delivered the `OnchainPosition` decoder and the
`/metrics-multi` JSON route, but neither was wired into the live
product path: websocket feeds still omitted `marketPdaHex`, the
trader panel could not consume per-market keeper metrics from the
browser, and operators had no single command to run a multi-market
keeper daemon with the JSON exporter attached. Wave 22 closes those
three end-to-end gaps.

### Added

- **`WebSocketFeedAdapter` position account decode.** Program-account
  notifications whose discriminator matches `Position` now route
  through wave-21 `decodeOnchainPosition`. The aggregator maintains
  a `positions` map keyed by position PDA hex; `aggregate()` emits
  `feed.positions[]` with `marketPdaHex` lifted from
  `position.market`. Closed positions (`status === 2`) are removed
  from the map. 1 fresh vitest (plus `buildAdapter` helper now
  registers the position discriminator so unknown-discriminator
  tests stay honest).
- **`MultiMarketFeedAdapter` per-market position routing.** Position
  updates route into `MarketState.positions` by
  `position.market.hex`; `aggregate()` merges `allPositions` across
  markets while preserving wave-20 `selectActiveMarketSnapshot`
  filtering semantics. 1 fresh vitest.
- **`keeperMetricsMulti` + `useKeeperMetricsMulti`.** New module
  parses wave-21 `/metrics-multi` JSON (`parseMetricsMultiJson`,
  `metricsJsonToKeeperState`, `mergeKeeperMetricsIntoFeed`).
  `appliedVolMilli / 1000 → appliedVol`; `volSamples < 3` with
  null `appliedVol` maps to `warming_up`. React hook polls
  `{VITE_KEEPER_METRICS_URL}/metrics-multi` every 4 s when the env
  var is set; offline dev leaves the hook inert. `App.tsx` merges
  keeper state before `selectActiveMarketSnapshot`. 5 fresh vitest
  cases.
- **`keeper-bot serve-multi` CLI mode.**
  `keeper-bot serve-multi <addr> <markets.toml> [max_passes]` loads
  a TOML registry, runs
  `run_loop_multi_market_leader_and_rpc_reconcile`, and binds
  `spawn_metrics_server_with_multi` so `/metrics-multi` is live on
  the same addr as `/metrics`. Compile-time delivery (no new Rust
  unit tests — behaviour covered by wave-21 HTTP/JSON tests).

### Changed

- **`frontend/src/feed/decode.ts`** — shared
  `onchainPositionToSummary` / `isDisplayablePosition` helpers
  (closed status = 2 filtered; untagged legacy positions still
  displayable for wave-9..18 compat).
- **`frontend/src/decoder/discriminators.ts`** — exports
  `MoleAccountDiscriminators.position` for adapter wiring.

### Verification

- `cargo test --workspace --all-targets` — **439 pass / 0 fail**
  (unchanged from wave 21; `serve-multi` is compile-only).
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `npx vitest run` (frontend) — **121 pass / 0 fail** (wave 21:
  114). Net new: 7 (5 keeperMetricsMulti + 1 websocket position +
  1 multiMarket position).
- `npx vitest run` (ops-toolkit/ts) — 33 pass / 0 fail
  (unchanged).
- `bash scripts/verify-test-counts.sh` — declared 439, observed
  439, slack 0.
- `bash scripts/verify-schema-parity.sh` — **103 / 103**.
- `bash scripts/verify-security-references.sh` — 32 / 32.
- Frontend `npm run build` + `tsc --noEmit` — clean (539 KB JS /
  159 KB gzip).

### Wave 23 priorities

1. **devnet integration matrix** — run `ops-toolkit prober
   --features solana-rpc` against a 2-market devnet deployment;
   capture `primary_backup_slot_diff` distribution.
2. **CI sandbox-external lanes** — `solana-rpc` feature compile
   smoke on self-hosted runner.
3. **Audit firm onboarding** (sandbox-external).

---

## [Wave 21] — 2026-05-24 — `OnchainPosition` mirror, live `solana-client` adapter, per-market JSON metrics, RPC retry + backup-slot-diff

Wave 20 stopped at the trait boundary: the `RpcAccountSource`
abstraction was wired but no production impl existed; per-market
metrics still travelled as Prometheus text scraping; positions
were filtered by market on the frontend but no adapter populated
`marketPdaHex`. Wave 21 closes those four production-readiness
gaps without breaking any wave-20 surface.

### Added

- **`keeper_decoder::OnchainPosition` Borsh mirror.** Byte-for-byte
  mirror of the on-chain `Position` PDA (247-byte Anchor account =
  8-byte disc + 239-byte body). Adds 23 new fields to
  `schema_descriptor_json()` (80 → 103 total). Unlocks two
  production paths: (1) the wave-22 frontend `websocketAdapter`
  can `accountSubscribe` to position PDAs and lift
  `position.market` into `PositionSummary.marketPdaHex` so the
  wave-20 `selectActiveMarketSnapshot` filter starts working on
  live data; (2) a future prober open-interest probe can decode
  every position via `getProgramAccounts` and feed
  `MarketFacts.current_total_principal` straight from the on-
  chain record. 5 fresh Rust tests (round-trip, body-length pin
  at 239 bytes, market pubkey integrity, status / direction byte-
  ordering, discriminator-strict path).
- **TypeScript `decodeOnchainPosition` / `decodeOnchainPositionWithDiscriminator`.**
  Hand-rolled Borsh layout in `frontend/src/decoder/onchain.ts`,
  pinned to the Rust source via `SCHEMA_DESCRIPTOR.OnchainPosition`
  (parity test asserts 103 fields across 6 structs). Public
  `OnchainPosition` interface uses `bigint` + hex `Pubkey32` — same
  shape conventions as the wave-14 `OnchainMarket` decoder. 6
  fresh frontend tests (encode → decode round-trip, body-length
  pin, market-pubkey integrity, truncated payload, discriminator
  mismatch, direction / status preservation).
- **`ops-toolkit::solana_rpc::SolanaRpcAccountSource`** — production
  `RpcAccountSource` impl backed by
  `solana_client::rpc_client::RpcClient`, gated behind the new
  `solana-rpc` cargo feature. Forwards `solana-rpc` to
  `keeper-rpc/solana-rpc` so the keeper-bot and prober share the
  same `solana-client = 4.0` dependency tree. Three constructors
  (`new`, `new_with_timeout`, `from_client`) plus a borrow of the
  underlying client for non-`RpcAccountSource` operations.
  `sleep_ms` wires to `std::thread::sleep` so the wave-21 retry
  backoff works in production. Default builds stay tiny — the
  Solana dep tree pulls in only when the feature is enabled.
- **RPC retry / backoff knobs on `RpcMarketFetcherConfig`.**
  `retry_attempts: u8` (default 0 — preserves wave-20 single-
  attempt behaviour) and `retry_backoff_ms: u64` (default 0 — no
  sleep) are wave-20-orthogonal. The fetcher's two RPC calls
  (`getMultipleAccounts` + `getSlot`) each flow through a
  `with_retry` helper that invokes `RpcAccountSource::sleep_ms`
  between attempts, so the production timing is correct without
  blocking host-side test runs (the test stub stores call counts
  + sleep durations in a `RefCell` for assertion).
- **Backup-RPC slot-diff sampling.** `RpcMarketFetcher::with_backup`
  attaches a second `RpcAccountSource` whose `getSlot` is called
  once per cycle. The absolute difference is recorded into
  `RpcFacts.primary_backup_slot_diff` (was hard-coded `0` in wave
  20). Backup `Err` collapses to `0` rather than poisoning the
  primary cycle — AlertManager's existing
  `RPC_PRIMARY_BACKUP_SLOT_LAG` rule fires only on real
  divergence between two healthy endpoints. Backup is
  **never** consulted for account reads.
- **`/metrics-multi` JSON metrics route + `KeeperMetrics::render_json_snapshot`.**
  Stable, append-only JSON object (camelCase keys: `ticksTotal`,
  `actionsSubmittedTotal`, `walletBalanceLamports`, …) renders
  the full wave-12 metric register without forcing the frontend
  to parse Prometheus text. `MarketRegistry::render_per_market_json`
  emits a `[{market, metrics}, …]` array; the new `serve.rs`
  route returns it (404 if no provider was wired, so wave-12
  single-market deployments are unaffected).
  `spawn_metrics_server_with_multi(addr, metrics, multi, shutdown)`
  is the new public API; the wave-12 `spawn_metrics_server`
  delegates with `multi = None`.

### Changed

- **`RpcAccountSource` trait** — added a default-no-op `sleep_ms`
  hook so out-of-tree implementers keep wave-20 source compat
  while the wave-21 retry path becomes test-deterministic. The
  production `SolanaRpcAccountSource` overrides it; the test
  stub records every call into a `Vec<u64>`.
- **`RpcFacts.primary_backup_slot_diff`** — promoted from a
  hard-coded `0` to a real measurement when a backup is
  configured. Existing fixtures unaffected.

### Verification

- `cargo test --workspace --all-targets` — **439 pass / 0 fail**
  (wave 20: 417). Net new tests: 5 (`OnchainPosition` mirror) +
  9 (`RpcMarketFetcher` retry + backup-RPC) + 7 (per-market JSON
  metrics + JSON snapshot shape) + 1 default-config wave-21 lock
  = 22.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo clippy -p ops-toolkit --features solana-rpc` — clean
  (compile-checks `SolanaRpcAccountSource` against
  `solana-client = 4.0`).
- `cargo clippy -p keeper-rpc --features solana-rpc` — clean.
- `npx vitest run` (frontend) — **114 pass / 0 fail** (wave 20: 108).
  Net new: 6 `OnchainPosition` decoder tests.
- `npx vitest run` (ops-toolkit/ts) — 33 pass / 0 fail
  (unchanged).
- `bash scripts/verify-test-counts.sh` — declared 417, observed
  439, slack 22 (refresh-warning surfaced; doc 20 §21 updated).
- `bash scripts/verify-schema-parity.sh` — **103 / 103** Rust
  fields documented in `SCHEMA-MAPPING.md` (was 80; +23 for
  `OnchainPosition`).
- `bash scripts/verify-security-references.sh` — 32 / 32.
- Frontend `npm run build` + `tsc --noEmit` — clean.

### Wave 22 priorities

1. **`websocketAdapter` per-position decode** — wire wave-21
   `decodeOnchainPosition` into the live adapter so
   `feed.positions[i].marketPdaHex` is populated end-to-end.
2. **Per-market `KeeperLoopMetrics` shape lock** — add a
   `KeeperLoopMetrics::per_market` struct so the per-market
   `MarketSlot.metrics` is consumed via the new `/metrics-multi`
   JSON contract instead of `MarketViewEntry.keeperState`'s
   wave-20 fallback.
3. **devnet integration matrix** — spawn `ops-toolkit prober
   --features solana-rpc` against a 2-market devnet deployment
   and capture the `primary_backup_slot_diff` distribution as a
   first-class observability artefact.
4. **CI sandbox-external lanes** — add the `solana-rpc` feature
   gate to the self-hosted SBF runner once it lands; until then,
   compile-only smoke is the wave-21 SLA.

---

## [Wave 20] — 2026-05-23 — multi-market position filter, live RPC prober fetcher, SOPS pipe

Wave 19 made multi-market support visible to the operator. Wave 20
closes the gap that **trader-side** state was still global, ships
the **live RPC fetcher** that turns the wave-19 prober daemon
from a fixture-only scaffold into a real on-chain probe, and
adds the `--markets-stdin` / `--env-from-file=PATH` flags that
let SOPS-decrypted configs flow through the daemon without ever
touching disk.

### Added

- **Per-market position filter on the trader panel.** `PositionSummary`
  gains an optional `marketPdaHex` field (mock generator + future
  websocket adapter populate it); `selectActiveMarketSnapshot`
  filters `feed.positions` to the active market in a new pure
  helper `filterPositionsByMarket`. Untagged positions stay (back-
  compat with wave-9..18 single-market mocks); tagged positions
  for *other* markets are dropped. 4 fresh `selectMarket` tests +
  4 standalone `filterPositionsByMarket` tests.
- **Per-market keeper metrics path.** `MarketViewEntry.keeperState?: KeeperState`
  carries the active market's keeper-bot metrics when the
  multi-market run loop publishes per-market metrics;
  `selectActiveMarketSnapshot` swaps `feed.keeper` to it (with a
  paused-flip overlay) so `KeeperPanel`'s tickSlot / appliedVol /
  cumulative counters reflect *this* market not the global mean.
  Falls back to the global `feed.keeper` shape when no
  per-market metrics are published yet. 2 fresh tests.
- **`ops-toolkit::rpc_fetcher` — live `MarketFetcher` for the
  prober daemon.** New `RpcAccountSource` trait abstracts
  `solana-client::RpcClient::get_multiple_accounts` + `getSlot`
  so the production fetcher is fully host-testable.
  `RpcMarketFetcher` bulk-fetches Market PDA + KeeperLeaderLock
  PDA in **one** `getMultipleAccounts` round-trip per market per
  cycle, decodes via `keeper_decoder::OnchainMarket` /
  `keeper_decoder::leader_lock::KeeperLeaderLock`, and assembles
  a complete `HealthContext` (paused / frozen / schema_version /
  leader_lock with cluster slot from `getSlot`, latency recorded
  into `RpcFacts.primary_get_slot_p95_ms`). 10 fresh unit tests
  covering decode happy paths, missing PDAs, transport failures
  on both calls, schema-version pass-through, and config
  defaults / overlays.
- **`ops-toolkit::cli_loader` — `--markets-stdin` and
  `--env-from-file=PATH`.** New host-testable CLI helper:
  `MarketsSource` (`File` | `Stdin`) + `EnvSource` (`Process` |
  `File` | `Inline`) + `load_registry` + `parse_env_file` +
  `extract_sources` + `read_process_stdin`. The prober and scan
  modes now both accept the new flags so SOPS pipelines work
  end-to-end:
  `sops -d markets.enc.toml | ops-toolkit prober --markets-stdin --env-from-file=/run/secrets/prober.env /var/lib/.../mole.prom /var/lib/.../prober.json 10 0`.
  Inline overlay falls back to process env for missing keys so
  long-lived non-secret values stay where they are. 16 fresh
  unit tests covering env-file shapes, flag parsing, source
  combination paths, and fallback semantics.

### Changed

- **`MarketViewEntry`** — gains `keeperState?: KeeperState`
  (wave-20 multi-market keeper metrics). All other fields
  unchanged.
- **`PositionSummary`** — gains `marketPdaHex?: string` (wave-20
  per-market position routing). All other fields unchanged.
- **`selectActiveMarketSnapshot`** — additionally filters
  `feed.positions` and (when present) swaps `feed.keeper` to the
  active market's `keeperState`. Single-market and pre-wave-19
  fixtures unaffected.
- **`ops-toolkit prober` / `ops-toolkit scan` CLI** — argument
  parsing migrates from raw `args().nth(N)` to
  `cli_loader::extract_sources`. Wave-19 positional shape stays
  100% backward-compatible: `ops-toolkit prober ./markets.toml /v/n/mole.prom /v/m/prober.json 10 0`
  continues to work byte-identical.
- **`ops-toolkit` Cargo.toml** — adds `keeper-decoder` (path) and
  a dev-dep on `borsh` so the live RPC fetcher tests can encode
  Anchor-discriminator-prefixed account fixtures without pulling
  the workspace `borsh` workspace dep into a new public surface.

### Verified

- `cargo test --workspace --all-targets`: **417/417 pass** (wave-19
  391 → +26 wave-20: 10 rpc_fetcher + 16 cli_loader).
- `cargo clippy --workspace --all-targets -- -D warnings`: **clean**.
- `cargo clippy -p keeper-rpc --features solana-rpc --all-targets -- -D warnings`: **clean**.
- `npm test -- --run` (frontend): **108/108 pass** (wave-19 101 →
  +7 wave-20 selectMarket extensions).
- `npm run typecheck`: clean (strict + `exactOptionalPropertyTypes`).
- `npm run build`: clean (535.27 KB JS / 157.48 KB gzip).
- `npx vitest run` (ops-toolkit/ts): 33/33 pass.
- All three governance verifiers (`verify-test-counts.sh`,
  `verify-security-references.sh`, `verify-schema-parity.sh`):
  **clean** after refreshing the doc-20 wave summary to declare
  417 tests.

### Sandbox-external (Wave 21 priorities)

1. **Live RPC client wiring.** Plug a real
   `solana_client::nonblocking::RpcClient` into `RpcAccountSource`
   behind a `solana-rpc` feature flag in `ops-toolkit`. Sandbox
   blocks `solana-client` build today; the trait is ready.
2. **Multi-market keeper-bot metrics publish.** `KeeperBot` run
   loop currently publishes a single `KeeperLoopMetrics`;
   wave-21 splits it per market so the wave-20
   `MarketViewEntry.keeperState` path actually carries
   per-market data instead of a fallback mirror.
3. **`websocketAdapter` position decoder.** Wire the wave-15
   borsh `OnchainPosition` mirror into the single-market adapter
   so live positions arrive with `marketPdaHex` populated. Mock
   path is already wired.
4. **Devnet 2-replica multi-market e2e** + CI SBF runner online +
   reject-matrix CU instrumentation + audit firm kickoff (carried
   forward from wave 18 / 19).

---

## [Wave 19] — 2026-05-23 — multi-market panels, prober daemon, env-var-driven configs

Wave 18 delivered the **infrastructure** for multi-market support
(shared registry, leader-lock grid, multi-market scan). Wave 19
turns that infrastructure into **operator-visible product**:

### Added

- **Multi-market trader / indexer / keeper panels** — `App.tsx`
  now renders the same three wave-12 panels with a per-market
  swap. The new `MarketSelector` (`frontend/src/panels/`) sits
  between the leader-lock grid and the panel tabs and renders
  one pill per configured market with a freshness dot
  (fresh / stale / unowned / uninitialised). Selection is
  persisted via the `useActiveMarket` hook (URL `?market=` query
  param + `localStorage["mole.activeMarket"]`); deep links share
  state and reloads remember the selection. Panels themselves
  remain untouched — `selectActiveMarketSnapshot` rewrites
  `feed.indexer / feed.keeper` to the active market's decoded
  view so `TraderPanel` / `IndexerPanel` / `KeeperPanel` keep
  consuming `feed.indexer` exactly as they did in wave 14.
- **`MultiMarketFeedAdapter` end-to-end decoding** — wave 18
  carried only raw `marketBytes / lockBytes`. Wave 19 decodes
  each market PDA into `MarketViewEntry.marketSummary`, fans
  out a single `onProgramAccountChange(programId)` subscription
  (gated on `discriminators` config) so sub-pool and
  dormant-bucket updates route to their owning market by
  inspecting `OnchainSubPool.market` and the parent sub-pool's
  pubkey. Result: `marketsView.entries` carries fully decoded
  per-market shapes (`subPools`, `dormantBuckets`,
  `projectedRecoveryOutstandingMicroUsdc`, `indexerSlot`) that
  drive every panel without re-fetching. The decoder helpers
  are extracted into `frontend/src/feed/decode.ts` so the
  wave-14 single-market adapter can converge on the same
  converters in a future wave.
- **`ops-toolkit::prober`** — wave-19 daemon scaffolding for the
  long-running prober. Periodic loop with pluggable
  `MarketFetcher`, `ProberClock`, `ProberSink` traits keeps the
  daemon I/O-free (zero `tokio` / `solana-client` /
  filesystem code in the loop) and fully synchronous in tests.
  Each cycle calls `scan_all_markets`, renders a unified
  Prometheus textfile (every metric line gets a `market="..."`
  label so `node_exporter`'s textfile collector keeps timeseries
  distinct), publishes a stable JSON snapshot, and propagates
  the worst exit code. Strict fail-closed policy on fetcher
  errors (no publication on a bad cycle so AlertManager's
  `for: 30s` rule trips on a textfile gap rather than stale
  all-Pass data). 9 host tests cover ok / hard-fail / degraded
  RPC / sink-failure / 3-cycle pacing / leader-lock routing
  and the prom relabeller's edge cases. `ops-toolkit prober
  <markets.toml> <prom-path> <json-path> [interval] [max-cycles]`
  binary mode lands today; live RPC fetcher is a thin
  `solana-client` shim added once the SBF runner comes online.
- **`MarketRegistry` env-var substitution** — `${VAR}` and the
  `$$` escape now expand against `std::env::var` (or a
  caller-supplied closure) BEFORE TOML parsing. Variable names
  must match `[A-Za-z_][A-Za-z0-9_]*`; unset / empty values
  raise `RegistryError::EnvVar { name }` so the operator sees
  the unresolved variable name in the error message instead of
  a confusing downstream "invalid pubkey" failure. Mirrored
  byte-for-byte in `ops-toolkit/ts/lib.ts::substituteEnvVars`
  so a single `markets.toml` template flows through Rust + TS
  without divergence; the SOPS workflow now decrypts secrets
  into env vars and `markets.toml` references them via
  `${VAR}`. 13 new Rust tests + 12 new TS tests cover
  passthrough / replace / `$$` / unset / empty / unclosed /
  empty-braces / invalid-chars / digit-prefix / underscore-prefix
  paths plus the `expected_leader` integration scenario.

### Changed

- **`MarketViewEntry`** — gained five wave-19 optional fields
  (`marketSummary`, `subPools`, `dormantBuckets`,
  `projectedRecoveryOutstandingMicroUsdc`, `indexerSlot`).
  Wave-18 fields stay required so existing renderers keep
  compiling; wave-19 panels read the new optional fields when
  present.
- **`App.tsx::buildAdapter`** — now passes
  `MOLE_ACCOUNT_DISCRIMINATORS` into `MultiMarketFeedAdapter`
  so sub-pool / dormant-bucket fan-out runs in production. The
  single-market `WebSocketFeedAdapter` path is unchanged.

### Verified

- `cargo test --workspace --all-targets` — **391 / 391 pass**
  (was 369; +22 wave-19 tests across `keeper-rpc`,
  `ops-toolkit::prober`).
- `cargo clippy --workspace --all-targets -- -D warnings` —
  clean.
- `cargo clippy -p keeper-rpc --features solana-rpc
  --all-targets -- -D warnings` — clean.
- `npm test` (frontend) — **101 / 101 pass** (was 83; +18
  wave-19 tests across `multiMarketAdapter`, `selectMarket`,
  `useActiveMarket`).
- `npm run typecheck` / `npm run build` — clean.
- `ops-toolkit/ts` `npx vitest run` — **33 / 33 pass** (was 21;
  +12 wave-19 tests).
- `scripts/verify-test-counts.sh` /
  `scripts/verify-security-references.sh` /
  `scripts/verify-schema-parity.sh` — all green.

### Wave 20 priorities (sandbox-external + product polish)

1. **TraderPanel multi-market positions filter** — current
   wave-19 path swaps `feed.indexer` only; `feed.positions` is
   still global. Filter positions by `marketPdaHex` so the
   trader view shows only positions for the active market.
2. **Live RPC `MarketFetcher` shim** — wire `solana-client`
   `getMultipleAccounts` into the prober binary's fetcher path
   once the self-hosted SBF runner lands.
3. **`devnet` 双副本切主 + 多市场冒烟** — carry-over from
   waves 17/18; needs cluster access.
4. **CI 自托管 SBF runner 上线** — sandbox-external.
5. **prober config SOPS 管线** — decryption hooks for
   `markets.toml.sops` calling `sops -d` then piping into the
   prober binary; today wave 19 ships only the `${VAR}` half
   of that workflow.
6. **审计 firm 启动** — sandbox-external.

---

## [Wave 18] — 2026-05-23 — multi-market native support: registry, runtime, frontend grid, ops-toolkit scan

### Added

- **`keeper-rpc::MarketRegistry`** — single source of truth for
  per-market config. TOML loader parses a deliberately tiny subset
  (`[[markets]]` array of bare-key string values) so no `serde` /
  `toml` dependency creeps into the lock-down ops VM. Each
  `MarketEntry` carries `symbol`, `program_id`, `market_pda`,
  pre-derived `lock_pda`, and an optional `expected_leader`.
  Symbol uniqueness, 16-byte ASCII cap, and base58 pubkey
  validation enforced at load time. 12 unit tests cover empty /
  duplicate / oversized / orphan-key / unknown-key / inline-comment
  paths.
- **`keeper-bot::MarketRegistry::from_config_with`** — bridge
  helper that fans a `keeper_rpc::MarketRegistry` out into a
  runtime registry of `MarketSlot`s. Each call to the user-supplied
  closure produces one slot (operator decides cadences, metrics
  registers, sub-pool sets). Closure errors propagate with the
  symbol context so multi-market boot failures are immediately
  attributable. 2 new unit tests; the existing `multi.rs`
  multi-market `run_loop` (acquired this session) still passes
  its 2 e2e tests.
- **Frontend `MultiMarketFeedAdapter`** — new
  `frontend/src/feed/multiMarketAdapter.ts` subscribes to N market
  PDAs + N lock PDAs + a shared cluster-slot poll. Aggregator
  emits `FeedSnapshot.marketsView.entries` (a `Map<symbol,
  MarketViewEntry>`). The legacy wave-17 single-market
  `keeperLeaderLockBytes` field mirrors the FIRST configured
  market's lock bytes for backward compat. 7 new tests verify
  empty / duplicate-symbol rejection, per-market subscription
  count, holdback-until-first-lock-update behaviour, and
  truncated-payload defence.
- **Frontend `marketRegistry.ts`** — JSON-encoded multi-market
  config parser keyed off `import.meta.env.VITE_MARKETS`. Same
  logical schema as the Rust TOML (symbol, programId, marketPda,
  optional lockPda + expectedLeader); 9 new unit tests cover
  malformed JSON / non-array / missing fields / dup symbols /
  oversized symbols / pubkey decode / lock-pda derivation.
- **Frontend `LeaderLockGrid`** — new top-of-page panel that
  replaces the wave-16 `LeaderLockBanner` when `feed.marketsView`
  is present. Renders one row per market (`symbol × status ×
  holder × slots`), flags `expected_leader` mismatches inline.
  Sorts deterministically (alphabetic by symbol) so two ticks
  always render the same DOM order. Pure `computeLeaderLockGridRows`
  helper covered by 5 unit tests including decode-error fallback.
- **`ops-toolkit::multi::scan_all_markets`** — multi-market
  scanner that fans the wave-12..17 21-check battery across every
  configured market. Auto-injects each entry's
  `expected_leader` into `LeaderLockFacts` so the prober closure
  doesn't have to thread that wiring. New
  `MultiMarketHealthReport` aggregates per-market reports plus
  the worst exit code; `render_json_multi` emits a
  `{worst_exit_code, markets: {symbol: HealthReport}}` object
  AlertManager can grep. 4 new unit tests.
- **`ops-toolkit` CLI `scan` mode** — `ops-toolkit scan
  ./markets.toml` loads a registry and runs the demo battery
  per market. Exits with the worst per-market severity tier so
  AlertManager pipelines work pre-prober.
- **`ops-toolkit/ts/keeper-leader-show-all.ts`** — wave-18 KL-09
  SOP. Reads a `markets.toml`, probes each market's lock PDA
  in one round-trip, prints a JSON or `--human` table with a
  worst-status field. Five severity tiers match the wave-17
  banner (pass / uninitialised / unowned / stale / mismatch);
  exit codes mirror the Rust ops-toolkit's P-tier logic.
- **`ops-toolkit/ts/lib.ts` TOML loader** — `parseMarketsToml`
  + `loadMarketsToml` mirror the Rust subset byte-for-byte so
  ops can ship one `markets.toml` consumed by both Rust and TS
  tooling. 7 new unit tests.

### Changed

- **`FeedSnapshot.marketsView` field** added to
  `frontend/src/types.ts` for the wave-18 grid. Wave-17
  single-market consumers continue to work — the field is
  optional and the multi-market adapter is opt-in via
  `VITE_MARKETS`.
- **`App.tsx`** branches on `feed.marketsView`: when present,
  renders `LeaderLockGrid` (with optional `expectedLeaders`
  from the parsed config); otherwise falls through to the
  wave-16 `LeaderLockBanner` for backward compat.
- **`crates/ops-toolkit/Cargo.toml`** now depends on
  `keeper-rpc` (default features) for the shared
  `MarketRegistry`. The dep tree gain is 0 new transitive
  crates beyond what `keeper` / `keeper-decoder` /
  `clearing-core` already pull (`borsh`, `sha2`, `thiserror`).

### Removed / Cancelled

- The wave-17 single-market `LeaderLockBanner` survives but is
  now the FALLBACK rendering; multi-market deployments use
  `LeaderLockGrid` exclusively.

### Verification

- `cargo test --workspace --all-targets` — 369 passed (was 347
  in wave 17 → +22 new).
- `cargo clippy --workspace --all-targets -- -D warnings` —
  clean.
- `cargo clippy -p keeper-rpc --features solana-rpc
  --all-targets -- -D warnings` — clean.
- `npm test` (frontend) — 83 passed, 0 failed (was 61 → +22).
- `npm run typecheck` (frontend) — clean.
- `npm run build` (frontend) — clean.
- `npx vitest run` (`ops-toolkit/ts`) — 21 passed (was 14 → +7).
- All three governance verifiers (`verify-test-counts`,
  `verify-security-references`, `verify-schema-parity`) pass.

### Remaining sandbox-external (Wave 19 priorities)

- devnet 双副本切主实战 — needs paid devnet + 2 hot wallets
- CI 自托管 SBF runner 上线 — needs solana-sbf labelled runner
- `expected_leader` injected via SOPS / vault encryption
- 审计 firm 启动 — Trail of Bits / OtterSec engagement
- 多市场 trader / indexer 面板（wave-19 product expansion）

---

## [Wave 17] — 2026-05-23 — frontend live leader-lock data, keeper-bot graceful release, ops-toolkit health checks + TS CLI, CI SBF stub

### Added

- **Frontend `WebSocketFeedAdapter` subscribes to `KeeperLeaderLock` PDA**
  — new `keeperLeaderLockPda?: PublicKey` option drives an
  `onAccountChange` subscription whose raw bytes flow through
  `FeedSnapshot.keeperLeaderLockBytes`. Optional `trackClusterSlot`
  poller fills `FeedSnapshot.currentSlot` from `getSlot()` so the
  banner ages the lock against the cluster clock, not the
  (possibly stale) keeper bot tick slot.
- **`App.tsx` decodes the live PDA bytes via wasm and feeds
  `LeaderLockBanner`** — wave-16's `view={null}` placeholder is gone;
  the banner now shows real holder identity / freshness / staleness
  on every PDA update. `useMemo` keeps decode cheap.
- **`keeper-bot try_graceful_release` + `release_on_shutdown`
  config** — when SIGTERM/SIGINT trips the shutdown flag and the
  bot was leader on the last tick, the run-loop publishes a single
  `keeper_leader_release` ix before returning
  `LoopOutcome::ShutdownSignal`. Failure to publish is a `warn!`,
  not fatal — the standby still recovers via natural takeover.
  Cuts maintenance gap from `≈30 s` (wave-15 takeover threshold)
  to `≤ 16 s` (standby reconcile cadence), often `< 5 s` in
  practice.
- **`crates/ops-toolkit` 3 new keeper-leader health checks** —
  `keeper_leader_lock_initialized` (P1: PDA missing →
  critical), `keeper_leader_lock_freshness` (P1: tiered Pass <
  60 % / Warn 60..=90 % / Critical ≥ 90 % of takeover threshold,
  Critical when no leader holds the lock), and
  `keeper_leader_lock_holder_matches_expected` (P2: opt-in via
  `LeaderLockFacts.expected_leader`; mismatch → critical with
  shortened-hex holder identifier in the message). All three
  return Pass + `leader_lock_enabled=0` when `HealthContext.leader_lock`
  is `None` so single-replica probers stay quiet by default.
- **`ops-toolkit/ts/` — 5 TypeScript CLI scripts** referenced by
  the wave-16 runbook. `keeper-leader-init.ts` (KL-01),
  `keeper-leader-show.ts` (read-only structured-JSON output for
  every § 6.5 SOP), `keeper-leader-acquire.ts` (KL-03,
  pre-flight refuses fresh acquire), `keeper-leader-heartbeat.ts`
  (KL-08 debug + reject-matrix harness), `keeper-leader-release.ts`
  (KL-02, pre-flight refuses non-holder release). Shared
  `lib.ts` re-derives discriminators from `sha256` and PDAs from
  `findProgramAddressSync` — zero Anchor IDL dependency, byte-
  identical to `keeper-decoder::ix`.
- **`KeeperPanel` `LeaderLockOpsCard` (wave-17 manual ops)** —
  three wallet-driven buttons (Acquire / Heartbeat / Release)
  build wave-15 keeper-leader instruction bytes via wasm and
  route them through the existing `WalletAdapter.signAndSubmit`
  path. Browser equivalent of the `ops-toolkit/ts/` CLI for
  incident response when the operator only has Phantom + a
  console window.
- **CI `ops-toolkit-ts` job** — typecheck + vitest run on every
  push, locks the wave-15 keeper-leader byte layouts.
- **CI `solana-program-test` job (gated)** — `runs-on:
  [self-hosted, solana-sbf]` + `if: false` until the runner is
  online; pre-positions `cargo build-sbf` + the wave-16
  `tests/keeper_leader.rs` reject-matrix harness so a single
  `if` flip enables it.

### Changed

- **`wasm-pack build crates/keeper-decoder --target web --features wasm`**
  — pkg artifact now exposes `encodeKeeperLeaderAcquire`
  (wave-16 only had heartbeat / release / decode). Frontend
  `wasmBuilder.ts::buildKeeperLeaderAcquireTx` switched from a
  hand-rolled DataView fallback to the wasm export, keeping the
  byte layout in lock-step with `keeper-decoder::ix` via the
  wave-15 single-source-of-truth contract.
- **`FeedSnapshot`** — added optional `keeperLeaderLockBytes?:
  Uint8Array` and `currentSlot?: bigint` so the live PDA + slot
  data can flow without breaking the wave-12+ shape. Mock
  adapters leave both undefined and the banner falls back to the
  wave-15 indexer slot reading.
- **`HealthContext`** — added optional
  `leader_lock: Option<LeaderLockFacts>` carrying initialized /
  has_leader / current_leader / last_heartbeat_slot /
  takeover_threshold_slots / current_slot / expected_leader.
  Default `None` keeps wave-12 probers behaviour-compatible.
- **`Docs/Planning/24-operator-runbook.md` § 6.5 SOPs** now
  reference real, runnable scripts under `ops-toolkit/ts/`
  (KL-01 init, KL-03 acquire, KL-02 release/show).

### Verification

- Rust: `cargo test --workspace --all-targets` **347/347 pass**
  (wave-16 baseline 338 + 9 new). 3 governance verifier scripts
  green (`verify-test-counts.sh`, `verify-security-references.sh`,
  `verify-schema-parity.sh`).
- Frontend: `npm run typecheck` clean; `npm test -- --run`
  **61/61 vitest pass** (wave-16 baseline 55 + 6 new); `npm run
  build` clean (wasm 46.06 KB; main bundle 519.74 KB / gzip
  154 KB).
- ops-toolkit/ts: `npm run typecheck` clean; `npx vitest run`
  **14/14 pass**.
- Clippy: workspace + `keeper-rpc --features solana-rpc` both
  `-D warnings` clean.

---

## [Wave 16] — 2026-05-23 — single-snapshot tick, on-chain `KeeperLeaderLock` reconcile + heartbeat, frontend leader banner, SBF reject matrix

### Added

- **`KeeperBot::tick_with_snap(&snap, …)`** — wave-9 / wave-10 tick
  pipeline (vol → predictor → scheduler → executor) refactored into
  a snapshot-only function. Wave 15's leader-gated run-loop now does
  one snapshot refresh per tick instead of two. Old `KeeperBot::tick`
  is a thin wrapper that owns the refresh — every existing caller is
  byte-identical. `Docs/Planning/20-…md § 16.1`.
- **`crates/keeper-rpc/src/leader_tx.rs`** — wave-16 keeper-leader tx
  module. `LeaderInstruction` (program_id + ix_data + accounts), three
  `build_keeper_leader_{heartbeat,acquire,release}` builders that wrap
  the wave-15 byte-exact encoders with the right Anchor account meta
  layout, `KeeperLeaderTxBuilder` trait + `MockKeeperLeaderTxBuilder`
  for host tests, and `fetch_keeper_leader_lock(fetcher, lock_pda)`
  that reads the on-chain PDA via any `AccountFetcher`, validates the
  Anchor `account:KeeperLeaderLock` discriminator, and decodes the
  49-byte body into the host state-machine type. Failure modes
  separately surfaced (`NotFound` / `Decode` / `Rpc`).
- **`crates/keeper-rpc/src/pda.rs::keeper_leader_lock_seeds(market)`**
  — wave-15 `[b"keeper_leader_lock", market]` PDA seeds re-exported
  in the same `PdaSeeds` shape as `sub_pool_seeds` /
  `dormant_bucket_seeds`.
- **`SolanaTxBuilder` implements `KeeperLeaderTxBuilder`** (under
  `--features solana-rpc`) — single tx per ix, signed by the keeper's
  payer wallet, returns `Ok(Some(sig))` / `Err(String)`.
- **`run_loop_with_leader_and_rpc_reconcile(…, rpc_cfg, leader_builder)`**
  in `keeper-bot::run` — leader-gated tick loop with on-chain reconcile
  cadence (`fetch_keeper_leader_lock` every N ticks, default 20) +
  heartbeat publish cadence (every M ticks AND immediately on the
  transition into leader, default 5). Reconcile / publish failures
  are logged but **never permanent** — the cached host mirror keeps
  the bot alive through transient RPC outages.
- **`LeaderRpcReconcileConfig`** — production defaults
  `reconcile_every = 20 ticks ≈ 16s @ 800ms`, `heartbeat_every = 5
  ticks ≈ 4s`, both well inside the wave-15 default
  `takeover_threshold_slots = 75 ≈ 30s`.
- **`frontend/src/panels/LeaderLockBanner.tsx`** — wave-16 ops surface.
  `deriveLeaderLockState(view, currentSlot)` is a pure function
  emitting `uninitialised | unowned | fresh{slotsUntilStale} |
  stale{slotsOverdue}`; the React banner renders all four states with
  a colour-coded badge + holder pubkey truncated to `aaaaaa…ffffff`.
  Wired into `App.tsx` above every panel; clock-skew (`currentSlot <
  lastHeartbeatSlot`) clamps elapsed to zero so we never render
  negative slot counts. CSS in `styles.css` (gray / yellow / green /
  red).
- **`programs/mole-option/tests/keeper_leader.rs`** — `solana-program-test`
  reject matrix skeleton for the four wave-15 keeper-leader ix.
  Coverage: happy path (init → first heartbeat acquires → self-refresh
  → release), heartbeat-by-other-while-fresh rejects with
  `KeeperLeaderHeldByOther`, acquire-while-fresh-self rejects with
  `KeeperLeaderAcquireWhileFresh`, release-by-non-holder rejects with
  `KeeperLeaderNotHolder`, observed_slot < recorded rejects with
  `KeeperLeaderClockSkew`. CU budget assertions (≤ 8 000 CU for
  heartbeat / acquire / release; ≤ 12 000 CU for init). Gated behind
  `--features _keeper_leader_program_test` so the workspace-excluded
  program crate doesn't pull SBF dev-deps into the default build —
  CI's SBF runner enables the feature with a one-line change to the
  GitHub Actions workflow when the toolchain bring-up lands.
- **`Docs/Planning/24-operator-runbook.md § 6.5`** — full keeper-
  leader operations chapter. KL-01 (init on new market), KL-02
  (planned leader handoff via release), KL-03 (failure handoff via
  acquire after takeover), KL-04 (deadlock triage), KL-05 (governance
  threshold change), KL-06 (CI reject matrix command), KL-07
  (frontend banner ops monitoring), KL-08 (resource model table).

### Changed

- `KeeperBot::tick` is now a thin wrapper around `tick_with_snap`.
  Behaviour byte-identical; existing callers unaffected.
- `run_loop_with_leader` (wave-15 host-mirror-only path) folded the
  pre-fetch into a single snapshot pass — wave 15's wave-15 prefetch
  + tick double-refresh no longer happens, eliminating the wave-15
  RPC-amplification factor.
- `Docs/Planning/20-攻坚开发进度与里程碑.md § 16.1–16.9` — full Wave 16
  entry: scope, decisions, evidence, evidence pointers.
- `Docs/Planning/23-on-chain-dormant-bridge.md § 18` — wave-16 cross-
  references.
- `README.md § Status` — bumped to wave 16; test counts updated.

### Verification

- `cargo test --workspace --all-targets` — **338 passing** (baseline
  wave-15 326 + wave-16 12 new tests: tick_with_snap 1 + leader_tx
  6 + reconcile 3 + e2e leader+RPC reconcile 2).
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo clippy -p keeper-rpc --features solana-rpc --all-targets
  -- -D warnings` — clean.
- `npm test` (frontend) — **55 passing** (wave-15 47 + wave-16
  LeaderLockBanner 8).
- `npm run typecheck` — clean. `npm run build` — clean (514.52 KB JS
  / 152.41 KB gzip + 45.87 KB wasm; no regression vs wave 15).
- All three governance verifiers (test counts, security references,
  schema parity) — green.
- `wasm-pack build crates/keeper-decoder --target web --features
  wasm` — unchanged (wave 16 doesn't touch the schema crate).

### Pending (out of sandbox)

- `cargo build-sbf` + `solana-program-test` reject matrix run — CI
  runner with SBF toolchain. Wave 16 ships the test code; the runner
  brings the runtime.
- Real keeper-bot ↔ devnet `KeeperLeaderLock` end-to-end loop — needs
  the program deployed first. `run_loop_with_leader_and_rpc_reconcile`
  is production-ready; only the cluster bring-up is missing.
- `KeeperLeaderLock` PDA `accountSubscribe` wired into the frontend
  `WebSocketFeedAdapter` so `LeaderLockBanner` shows live data — wave
  17 work, surface and decoders all ready.

---

## [Wave 15] — 2026-05-22 — wasm-pack tx-builder, on-chain `KeeperLeaderLock`, real `signAndSubmit`

### Added

- **`crates/keeper-decoder` → `wasm-pack`-buildable web bundle.** The
  schema-only crate now exposes a `wasm` Cargo feature that pulls in
  `wasm-bindgen` + `js-sys` + `console_error_panic_hook` +
  wasm-targeted `getrandom`, plus a `cdylib` crate-type so
  `wasm-pack build crates/keeper-decoder --target web --features wasm`
  produces a clean `pkg/` (45.87 KB `keeper_decoder_bg.wasm` + ESM
  wrapper). Frontend consumes it via
  `"keeper-decoder": "file:../crates/keeper-decoder/pkg"` in
  `frontend/package.json`. The wave-14 hand-rolled `@coral-xyz/borsh`
  TypeScript decoders are kept as a byte-level **parity oracle** for
  the wasm encoder (vitest diffs every encoded ix against the TS
  reference; schema drift fails CI immediately). See
  `Docs/Planning/20-…md § 15.1 / § 15.2`.
- **`crates/keeper-decoder/src/leader_lock.rs`** — `KeeperLeaderLock`
  state machine moves from `chain-mirror` into the schema crate so
  the same Borsh layout drives the host keeper bot, the on-chain
  Anchor account, and the frontend (via wasm). Fixed 49-byte body
  layout (`bool has_leader + [u8; 32] current_leader + u64
  last_heartbeat_slot + u64 takeover_threshold_slots`) — chosen over
  `Option<Pubkey32>` to keep Borsh + Anchor `space` bookkeeping
  trivial. Full state-machine surface
  (`fresh / held_by / try_heartbeat / try_release` + 5
  `HeartbeatOutcome` variants + 3 `ReleaseOutcome` variants) plus 13
  unit tests *and* a property test pinning the "at most one holder
  under arbitrary heartbeat sequence" invariant.
- **`crates/keeper-decoder/src/ix.rs`** — pure-Rust Anchor instruction
  encoders for `open_position`, `close_position`,
  `keeper_leader_acquire`, `keeper_leader_heartbeat`,
  `keeper_leader_release`. `instruction_discriminator(name)` /
  `account_discriminator(name)` powered by `sha2` (`sha256("global:<ix>")[..8]`,
  `sha256("account:<TypeName>")[..8]`). Golden discriminator vectors
  pinned in unit tests so a future schema bump breaks the test
  before it breaks production.
- **`crates/keeper-decoder/src/wasm_bridge.rs`** — `wasm-bindgen` FFI
  surface. JS-facing API: `wasmInit / instructionDiscriminator /
  accountDiscriminator / encodeOpenPosition / encodeClosePosition /
  encodeKeeperLeaderAcquire / encodeKeeperLeaderHeartbeat /
  encodeKeeperLeaderRelease / decodeKeeperLeaderLock /
  keeperLeaderLockSeedPrefix`. `KeeperLeaderLockView` marshals u64 →
  bigint and `[u8; 32]` → `Uint8Array` so UI code can render holder
  identity + slots-until-stale without round-tripping through Rust.
- **`programs/mole-option`** — on-chain `KeeperLeaderLock` PDA +
  4 instruction handlers:
  - `initialize_keeper_leader_lock` (permissionless, one-time per
    market, init `seeds = [b"keeper_leader_lock", market.key()]` +
    `space = LEN`).
  - `keeper_leader_acquire(KeeperLeaderHeartbeatArgs)` — strict
    claim-stale path, rejects with `KeeperLeaderAcquireWhileFresh`
    when called against a fresh lock (HA replica use case).
  - `keeper_leader_heartbeat(args)` — all-paths heartbeat: fresh
    acquire / same-signer refresh / stale takeover / wrong-holder
    reject. Behaviour matches `keeper_decoder::leader_lock::try_heartbeat`
    1:1.
  - `keeper_leader_release()` — graceful release, holder-only.
  Plus 5 new program errors: `KeeperLeaderHeldByOther`,
  `KeeperLeaderClockSkew`, `KeeperLeaderNotHolder`,
  `KeeperLeaderNotHeld`, `KeeperLeaderAcquireWhileFresh`. Anchor
  handler additionally validates `args.observed_slot ≤ Clock::slot`
  to block keepers from stamping future slots to extend leadership.
- **`crates/keeper-bot/src/leader.rs`** — `LeaderPolicy` trait +
  `HostMirrorLeaderPolicy` (caches the on-chain `KeeperLeaderLock`,
  runs `try_heartbeat` against the cached mirror each tick,
  `record_outcome` syncs the mirror with chain-confirmed heartbeats,
  `reconcile()` overrides on RPC fetch). `FixedLeaderPolicy` for
  deterministic tests. 6 host tests cover holder/non-holder/stale
  takeover/reconcile/record-outcome/fixed-policy paths.
- **`crates/keeper-bot::run_loop_with_leader`** — leader-gated tick
  loop variant. Each tick prefetches a snapshot, computes
  `current_slot = max(sub_pool.last_sync_slot)`, calls
  `policy.should_submit(current_slot)`, sets the
  `keeper_leader_status` Prometheus gauge to `Leader` / `Standby`
  accordingly, and skips dispatch when the gate denies. Wave-12
  file-lock callers continue using `run_loop_with_factory` for
  unchanged behaviour.
- **`frontend/src/tx/wasmBuilder.ts`** — TypeScript adapter wrapping
  the wasm encoders + decoder. `loadKeeperDecoder()` lazy-init,
  `buildOpenPositionTx / buildClosePositionTx /
  buildKeeperLeaderHeartbeatTx / buildKeeperLeaderReleaseTx` produce
  `Uint8Array` borsh bytes ready for `wallet.signAndSubmit`,
  `decodeKeeperLeaderLockBytes` exposes a JS-friendly
  `KeeperLeaderLockView`. 16 vitest tests cover discriminator
  stability + golden vectors, byte-level parity with the wave-14
  hand-rolled `encode.ts` encoder, fixed-size lock decoding (49-byte
  body + 57-byte padded Anchor account), and PDA seed bytes.
- **`frontend/src/panels/TraderPanel.tsx`** — open/close buttons now
  go through `buildOpenPositionTx / buildClosePositionTx` →
  `wallet.signAndSubmit` → renders the returned signature inline.
  WASM module is lazy-loaded with explicit `wasmReady / wasmError`
  state and disables the buttons until ready.
- **`.github/workflows/ci.yml`** — new `wasm-pack` job: installs
  `wasm-pack 0.13.1 --locked`, runs `wasm-pack build crates/keeper-decoder
  --target web --features wasm`, greps the wrapper JS for the
  expected wave-15 symbols, uploads `pkg/` as an artifact. The
  `frontend` job now `needs: [wasm-pack]` and `download-artifact`s
  `pkg/` before `npm ci`, so the production build embeds the same
  wasm bytes the wasm-pack job validated.

### Changed

- **`crates/chain-mirror/src/leader_lock.rs`** — converted to a thin
  re-export shim over `keeper_decoder::leader_lock::*` so existing
  `chain_mirror::leader_lock::*` import sites keep working.
  3 bridge integrity tests pin the re-export.
- **`programs/mole-option/src/lib.rs`** — wires the 4 new
  keeper-leader instructions into the Anchor `#[program]`.
- **`crates/keeper-decoder/Cargo.toml`** — `[lib] crate-type =
  ["cdylib", "rlib"]` + optional `wasm` feature. Default host build
  unaffected (zero new transitive deps).
- **`frontend/package.json`** — adds `keeper-decoder` file: dep +
  `wasm-build` convenience npm script.

### Verification

- `cargo test --workspace --all-targets` — **325+ pass** (wave-14
  baseline 274 plus keeper-decoder leader-lock 13 + ix 9 + keeper-bot
  leader 6 + chain-mirror bridge 3 + keeper-bot snapshot helper 1 +
  Anchor program tests on the off-workspace member).
- `npm test` (frontend) — **47 pass** (wave-14 baseline 31 +
  wasmBuilder 16).
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `npm run typecheck` clean (strict + `exactOptionalPropertyTypes` +
  `noUncheckedIndexedAccess` + `verbatimModuleSyntax`).
- `npm run build` — 512.40 KB JS (151.93 KB gzip) + 45.87 KB
  `keeper_decoder_bg.wasm` embedded.
- `wasm-pack build crates/keeper-decoder --target web --features
  wasm` — runs cleanly in-sandbox (after `rustup update stable` to
  1.95.0 and explicit `PATH=$HOME/.rustup/toolchains/...`); CI
  pipeline reproduces the same.
- 3 governance verifier scripts — all green.

### Pending — not sandbox-completable

- `cargo build-sbf` + `solana-program-test` for the 4 new
  `keeper_leader_*` ix (reject matrix + CU measurement). The host
  state machine's property test already proves the "at most one
  holder under arbitrary heartbeat sequence" invariant; the on-chain
  ix is the same logic materialised in Anchor.
- Real keeper-bot ↔ devnet `KeeperLeaderLock` end-to-end loop —
  needs the program deployed first. The `run_loop_with_leader` +
  `LeaderPolicy::record_outcome` hooks already model the production
  wiring; only the RPC binding remains.
- Audit firm engagement (carried over from wave 13/14).

---

## [Wave 14] — 2026-05-22 — keeper-decoder schema split + real frontend wiring

### Added

- **`crates/keeper-decoder`** — new schema-only crate. The five
  `Onchain*` Borsh structs (`OnchainSubPool`, `OnchainDormantBucket`,
  `OnchainDistEntry`, `OnchainDistributionLedger`, `OnchainMarket`)
  plus their decode / encode helpers move out of `keeper-rpc` into a
  zero-Solana-dep crate that compiles cleanly on
  `wasm32-unknown-unknown` (verified locally; the CI YAML adds the
  `rustup target add wasm32-unknown-unknown` + `cargo build --target
  wasm32-unknown-unknown --release -p keeper-decoder` step so wave-15
  `wasm-pack` lands without further refactor). 12 host tests
  including byte-for-byte round-trips, truncated/empty/malformed
  payload paths, discriminator-strict path, and an 80-field
  `schema_descriptor_json()` parity lock.
- **`crates/keeper-rpc/src/accounts.rs`** — converted to a thin
  re-export shim so every external `keeper_rpc::accounts::OnchainSubPool`
  call site keeps working unchanged. Three new bridge tests pin the
  re-export wiring.
- **`scripts/verify-schema-parity.sh`** — points at the new
  authoritative source `crates/keeper-decoder/src/lib.rs`. Same
  80-field check, decoupled from the host adapter crate.
- **`frontend/src/decoder/onchain.ts`** — TypeScript Borsh decoder
  (~600 LOC) mirroring the Rust schemas via `@coral-xyz/borsh`
  primitives + `buffer-layout::blob` for fixed-size arrays. Public
  API exposes `bigint` / `Pubkey32 { hex }` shapes; raw `BN` values
  stay internal. A `SCHEMA_DESCRIPTOR` constant pins field order
  against `keeper_decoder::schema_descriptor_json()`.
- **`frontend/src/decoder/discriminators.ts`** — sync sha256 (via
  `@noble/hashes`) computes the canonical Anchor account
  discriminators (`sha256("account:<TypeName>")[..8]`) at module
  load. No async startup dance.
- **`frontend/src/feed/websocketAdapter.ts`** — real implementation
  replacing the wave-12 placeholder. Subscribes to
  `Connection.onAccountChange` (market PDA) + `onProgramAccountChange`
  (sub-pools, dormant buckets), routes by 8-byte discriminator,
  decodes via the wave-14 TS Borsh layouts, holds back snapshots
  until the market arrives, and dispatches a fully-populated
  `FeedSnapshot` on every aggregator update. Injectable
  `connectionFactory` keeps the adapter unit-testable without a
  live websocket.
- **`frontend/src/wallet/windowWalletAdapter.ts`** — real
  implementation replacing the wave-12 stub. `signAndSubmit` calls
  `window.solana.signAndSendTransaction` and maps every error class
  the wallet can produce to a structured `WalletSignError.kind`:
  `WalletNotConnected`, `NoTxBytes`, `ProviderMissing`,
  `ProviderUnsupported`, `UserRejected`, `ProviderError`. Trader
  panels can now branch on `kind` to render an actionable banner.
- **`frontend/src/App.tsx`** — `buildAdapter()` reads `VITE_RPC_URL`
  / `VITE_MOLE_PROGRAM_ID` / `VITE_MARKET_PDA` env vars; falls back
  to `MockFeedAdapter` if any is missing so the default `npm run
  dev` flow keeps working without on-chain configuration.
- **`frontend/vitest.config.ts`** — vitest setup: `jsdom` default
  env (with per-file `// @vitest-environment node` overrides for
  pure-Node tests). New scripts `npm run test` + `npm run test:watch`.
- **Frontend deps** — `@solana/web3.js@^1.98`, `@coral-xyz/borsh@^0.30`,
  `@noble/hashes@^1.5`, `vitest@^2.1`, `jsdom@^25`,
  `@types/{node,bn.js}`.

### Fixed

- **Stale `.tsc -b` artifacts** — removed every committed `.js` next
  to a `.ts` (the wave-13 build had emitted `tsc -b` outputs in-tree
  and Vite's resolver was picking the stale JS over the live TS).
  `frontend/tsconfig.json` now sets `noEmit: true` and the build
  script is `tsc --noEmit && vite build`, so no future emit can
  shadow source.
- **`scripts/verify-schema-parity.sh`** — switched authoritative
  source to `crates/keeper-decoder/src/lib.rs` after the wave-14
  schema crate split. SCHEMA-MAPPING.md updated accordingly.

### Tests

- Rust workspace: **274/274 pass** (baseline 262 + keeper-decoder
  12). Clippy `-D warnings` clean across default and `solana-rpc`
  feature surfaces.
- Frontend: **31/31 vitest pass** (12 decoder + 7 ws-adapter +
  12 wallet-adapter).
- TypeScript strict mode (`strict` + `exactOptionalPropertyTypes` +
  `noUncheckedIndexedAccess` + `verbatimModuleSyntax`) clean.
- Production `vite build` produces a 506 KB / 149 KB gzipped JS
  bundle.

### Notes for auditors

- The wave-14 TS Borsh schemas in `frontend/src/decoder/onchain.ts`
  are a **stop-gap**; the long-term plan is wave-15 `wasm-pack
  build` of `keeper-decoder` so the frontend imports the Rust
  schemas verbatim. Until then, `crates/keeper-decoder::tests::
  schema_descriptor_lists_every_struct_and_field` and
  `frontend/src/decoder/onchain.test.ts::SCHEMA_DESCRIPTOR
  enumerates exactly 80 fields...` form a manual two-sided lock.
- `WebSocketFeedAdapter`'s `connectionFactory` injection is *also*
  the seam where wave-15 / 16 add reconnect-with-backoff
  (currently a single attempt; `maxBackoffMs` is wired through but
  unused). The unit tests exercise the structural path; live
  reconnect behavior is verified manually against devnet today.

---

## [Wave 13] — 2026-05-22 — Audit-readiness governance

### Added

- **`.github/workflows/ci.yml`** — full CI pipeline running
  `cargo build/test/clippy` (default features and the
  `solana-rpc` feature), `npm run typecheck` + `npm run build` for
  the frontend, the `ops-toolkit demo` smoke pair (exit 0 / exit 4),
  and a one-tick `keeper-bot serve` smoke. Backed by `actions/cache`
  on the cargo registry + target tree.
- **`scripts/verify-security-references.sh`** — every test reference
  in `SECURITY.md` (32 invariants → live test functions) is
  word-boundary-grepped against the codebase; a renamed test fn
  trips this gate on the next PR.
- **`scripts/verify-test-counts.sh`** — parses the declared test
  count from `Docs/Planning/20-…md` (current: 262/262) and asserts
  `cargo test --workspace` reports the same number.
- **`scripts/verify-schema-parity.sh`** — parses every `pub <field>:`
  on the `Onchain*` Borsh mirrors in `crates/keeper-rpc/src/
  accounts.rs` (80 fields across 5 structs) and asserts each appears
  as a word-boundary entry in `Docs/SCHEMA-MAPPING.md`.
- **`Docs/SCHEMA-MAPPING.md`** — explicit mapping of every Rust
  schema field to its TypeScript surface in
  `frontend/src/types.ts` — or an `omitted (rationale)` row. This
  file is the structural counterpart to `SECURITY.md` (invariants
  ↔ tests) for the schema layer (Rust ↔ TS).
- **`CONTRIBUTING.md`** — onboarding for external contributors and
  audit firms: how to run the gates locally, what the CI pipeline
  enforces, and where the trust boundaries live.
- **`CHANGELOG.md`** (this file) — wave-by-wave externally-readable
  history.

### Fixed

- **`SECURITY.md`** — wave 12 published 27 invariants, but 20 of the
  test references pointed at file paths and function names that
  didn't exist in the actual codebase (the file used aspirational
  names from the original planning docs rather than the names tests
  actually grew into). All 20 are now repointed at live tests:
  - `CORE-1..4`, `CORE-5..8`, `CORE-7..8` → tests in
    `crates/clearing-core/tests/{properties,dormant_cycles,
    dormant_lazy_equivalence,lazy_ledger}.rs` and
    `crates/protocol-harness/tests/{random_workload,
    rotation_focused}.rs`.
  - `ONCH-1..3` → host-side tests in `crates/chain-mirror/src/
    tests.rs` (which faithfully mirror the Anchor rejection
    semantics) and the existing lazy-distribute idempotence test.
  - `GOVN-1..6` → all eight `paused_blocks_*` tests in
    `crates/clearing-core/tests/safety_gates.rs`, schema-mismatch
    coverage in `crates/keeper-rpc/src/snapshot.rs`, and the
    governance-bump and migrate-position tests in
    `crates/chain-mirror/src/tests.rs`. GOVN-5/6 now correctly
    reference `bump_market_schema_version` /
    `migrate_position` — the names of the actual ix in this
    codebase, not the placeholder names from the wave-9 plan.
  - `KEEP-1..5` → predictor and vol-estimator tests in
    `crates/keeper/src/lib.rs`.

### Notes for auditors

- `SECURITY.md`'s claimed-vs-actual gap is the kind of dilution the
  CI verifier scripts exist to prevent going forward. We're
  publishing this one openly rather than silently overwriting because
  audit transparency means showing your gaps.
- Wave 14 will land the actual Solana-toolchain-bringup workflow
  (`solana-program-test`, BPF compile, governance reject matrix);
  this requires a runner with stable bandwidth that the development
  sandbox couldn't provide.

---

## [Wave 12] — Production daemon + ops automation + audit readiness

### Added

- **`crates/keeper-bot::{metrics, serve, run}`** — production
  wrapping of the wave-10 `KeeperBot::tick`. 13 Prometheus metrics
  (atomic, lock-free), a hand-rolled HTTP/1.1 listener serving
  `/metrics` and `/healthz`, and a permanent-vs-transient error
  classifier (`is_transient`). 22 lib unit tests + 5 e2e integration
  tests.
- **`crates/ops-toolkit`** — automation of the
  `Docs/Planning/24-operator-runbook.md §2` 18-row health dashboard.
  18 pure check functions, 3 reporters (human / JSON /
  Prometheus textfile), severity-keyed exit codes (P0 critical → 4,
  P1 critical → 3, …). 26 unit tests.
- **Frontend wave-12 stage 1** — `feed/` and `wallet/` modular
  adapters: `MockFeedAdapter` / `WebSocketFeedAdapter` (placeholder)
  and `MockWalletAdapter` / `WindowWalletAdapter` (Phantom /
  Backpack / Solflare detection). `?feed=live` URL parameter
  switches adapters without changing panel code.
- **`SECURITY.md`** — root-of-repo security policy: A-1..A-7 threat
  model, 27 invariants across CORE / ONCH / GOVN / KEEP / OPS, 5
  trust assumptions, vulnerability disclosure SLA + bug bounty.

### Tests

- 262 total tests, 273 with `--features solana-rpc`. `clippy -D
  warnings` clean across the workspace.

---

## [Wave 11] — Solana RPC integration + operator runbook + frontend MVP

### Added

- **`crates/keeper-rpc::solana`** (gated on `--features solana-rpc`)
  — production `SolanaRpcAccountFetcher` and `SolanaTxBuilder`
  binding `solana-client 4.0`. Pubkey32 ↔ `solana_pubkey::Pubkey`
  byte-equal round-trip, AccountMeta flag preservation, base64
  account-data decoder. 11 unit tests.
- **`Docs/Planning/24-operator-runbook.md`** — comprehensive
  operator runbook: roles, authority matrix, daily-health
  dashboard, SOPs for keeper bot / market pause / global shutdown /
  schema upgrade, IR-01..05 incident playbooks, alert thresholds.
- **`frontend/`** — React + Vite + TypeScript MVP with three
  panels (Trader / Indexer / Keeper Console), mock feed simulating
  evolving on-chain state, dark console-feel UI.

### Fixed

- Aligned `solana-client 4.0` dependency tree by switching from the
  monolithic `solana-sdk` to granular crates (`solana-pubkey`,
  `solana-transaction`, `solana-instruction`,
  `solana-commitment-config`, `solana-keypair`, `solana-signer`,
  `solana-hash`).

### Tests

- 216 host-only / 227 with feature.

---

## [Wave 10] — Auto-tuning keeper + production RPC scaffold + daemon

### Added

- **`RealizedVolatilityEstimator`** — time-weighted rolling σ̂
  estimator with `[0.05, 5.0]` clamp, sample windowing by count and
  age, warm-up period gating, and an `apply_to_predictor_config`
  hook that auto-tunes `RotateRiskPredictor` once warm.
- **`KeeperLoop`** — synchronous `tokio`-independent state machine
  driving the keeper bot's tick: fetch prices → vol estimator →
  apply σ̂ → init-hint generation → scheduler → executor.
  `KeeperLoopMetrics` (submitted / failed / skipped) tracked
  per-tick.
- **`crates/keeper-rpc`** — offline-testable RPC abstraction:
  Borsh-decoded account mirrors, `AccountFetcher` /
  `MockAccountFetcher`, `ChainSnapshot` (implements
  `KeeperChainView`), `TxBuilder` / `MockTxBuilder`,
  `RpcExecutor`. Anchor instruction discriminators pinned via a
  sha2-based compile-time self-test.
- **`crates/keeper-bot`** — runnable daemon (`bin` + `lib`)
  wiring the above into a polled cycle. 5 e2e integration tests.

---

## [Wave 9] — Governance lockdown + schema versioning

### Added

- `GlobalConfig.paused_globally` and per-`Market.paused` flags
  blocking every funds-touching ix.
- `Market.frozen_new_position` for "no new opens, existing
  closeable" mode.
- `bump_market_schema_version` + `migrate_position` for
  zero-downtime schema upgrades; client `schema_version_current`
  mismatch rejects every ix with `SchemaVersionMismatch`.

---

## [Wave 8] — Per-block O(1) clearing

### Added

- `mark_to_market_distribution` updates only `SubPool` aggregate
  fields per block; per-position settlement is lazy.
- `DistributionLedger` cumulative checkpoints with
  `cum_at(t2) - cum_at(t1) ≡ Σ_{t∈(t1,t2]} block_distribution[t]`
  invariant.

---

## [Wave 7] — Lazy migration + dormant lifecycle

### Added

- Lazy migration from active → recovery state when a sub-pool
  rotates; eager-vs-lazy equivalence under random workloads.
- DormantBucket lifecycle: init (keeper) → claim (user) →
  drain (recovery) → close (keeper).

---

## [Wave 6] — Loss recovery + locked-loss locking

### Added

- Loss-locking semantics: realized losses are deducted from
  `pool_equity` and parked in a `pending_recovery` slot until
  recovery shares pay it down.
- `Σ(loss_locked) ≤ Σ(deposited)` invariant.

---

## [Wave 5] — Sync barriers + idempotency

### Added

- `sync_pre_open` idempotency: calling twice on the same
  generation is a no-op.

---

## [Wave 4] — Indexer parity

### Added

- Off-chain indexer that replays on-chain events and reproduces
  bucket / sub-pool state; aggregated payouts match within
  bounded drift across random workloads.

---

## [Wave 3] — Position lifecycle + atomic revert

### Added

- Position open / close round-trip with zero-PnL parity.
- Failed close paths leave chain state byte-untouched (revert
  is atomic).

---

## [Wave 2] — Engine bring-up

### Added

- `clearing-core` engine: SubPool, Position, DormantStore,
  invariants module.
- Adversarial gates: envelope deviation rejection, force-close
  acknowledge gate, dilution-safety floor.

---

## [Wave 1] — Math foundations

### Added

- `crates/molemath`: checked fixed-point primitives, signed-PnL
  computation, price-move-bps helper, Q-prefix arithmetic.
