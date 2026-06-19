# MoleOption

> Solana-native, never-liquidated, risk-capped leveraged market.

This repository contains:

| Path | What |
|------|------|
| `Docs/` | Whitepaper + 19 planning docs (auditing, math model, security review, …) |
| `crates/molemath/` | Fixed-point money-grade math primitives |
| `crates/clearing-core/` | Reference implementation of the O(1) directional-equity-pool **shares model**, with structured `EngineEvent` emission |
| `crates/simulation/` | Whitepaper §3 per-position **proportional clearing oracle** (offline ground truth) |
| `crates/indexer/` | Off-chain replay of `EngineEvent` → per-position `locked_loss / realized_profit_balance` view (front-end ledger truth) |
| `crates/pyth-adapter/` | Host-testable Pyth Pythnet v2 price account validator (program id / magic / status / expo / age / confidence / sign) |
| `crates/protocol-harness/` | End-to-end protocol simulator: vault accounting + multi-sub-pool clearing + indexer in lock-step. Property tests cover conservation, vault decomposition, indexer parity, adversarial paths |
| `crates/chain-mirror/` | **Wave 5+8.** Host-side replica of the Anchor account-runtime. Models every on-chain account (`SubPool`, `DormantBucket`, `DistributionLedger`, `Position`) as an owned struct, bridges them through `clearing_core::pack_dormant_store` / `unpack_dormant_store` per instruction, and emulates Solana tx-revert semantics. Driven against `protocol-harness` for byte-equal parity property tests under Eager / Lazy / Stress workloads. **Wave 8 strict-PDA-lifecycle mode** mirrors anchor's `pack_direction` Pass 1/2/3 and `BucketSlotExhausted` semantics — keeper must `pre_init_dormant_bucket` before each rotate. |
| `crates/keeper/` | **Wave 8+9+10.** Pure-Rust keeper scheduler core: `KeeperChainView` snapshot → priority-ordered `KeeperAction` queue. **Wave 9** added `RotateRiskPredictor` (one-touch GBM model, A&S Φ approximation) + `ActionExecutor` trait + `DryRunExecutor`. **Wave 10** added `RealizedVolatilityEstimator` (time-weighted σ̂ from oracle history, auto-feeds `PredictorConfig`) + `KeeperLoop` (synchronous tick state machine with `KeeperBotEnvironment` trait — host-testable end-to-end without a runtime). |
| `crates/keeper-rpc/` | **Wave 10+11.** Solana RPC adapter for the keeper crate. Default-features build is host-only: borsh-decoded `Onchain{Market,SubPool,DormantBucket,DistributionLedger}` mirrors + `AccountFetcher` trait + `ChainSnapshot` (implements `KeeperChainView`) + `TxBuilder` trait + `RpcExecutor`. Anchor IX discriminators are hard-coded `pub const`s pinned by a `sha2`-based self-test (`discriminator_constants_match_sha256_of_anchor_namespace`) so a program-side rename can never silently desync. **Wave 11** wired the production `solana-rpc` feature (`SolanaRpcAccountFetcher` + `SolanaTxBuilder` over `solana-client 4.0`'s sync `RpcClient`, with `getProgramAccounts` filtering routed through `client.send` to bypass the missing 4.0 high-level API). |
| `crates/keeper-bot/` | **Wave 10+12.** Runnable keeper daemon: `KeeperBot::tick(fetcher, ctx, builder, …)` wires `ChainSnapshot` + `Scheduler` + `RotateRiskPredictor` + `RealizedVolatilityEstimator` + `RpcExecutor` into one polled cycle. `cargo run -p keeper-bot smoke` is an offline single-tick runner; **wave 12** added `cargo run -p keeper-bot serve <addr> <max_ticks>` which spins up a hand-rolled HTTP exposition server (`/metrics` Prometheus 0.0.4 / `/healthz` 503 → 200), structured JSON logs via `tracing-subscriber`, SIGINT/SIGTERM graceful shutdown via `ctrlc`, and `run_loop_with_factory` that classifies governance errors as permanent (loop exits) vs RPC transport errors as transient (loop continues). Zero tokio / hyper dependency. |
| `crates/ops-toolkit/` | **Wave 12.** Operations health prober. 18 pure-function checks `(HealthContext) -> CheckResult` matching `Docs/Planning/24-operator-runbook.md` § 2 line-for-line. `Severity (P0..P3) × Status (Pass/Warn/Critical)` → exit code 0/1/2/3/4 strict mapping. Triple output (JSON / Prometheus textfile / human-readable). Zero serde dependency — JSON is hand-rolled. `cargo run -p ops-toolkit demo` exits 0 (all 18 pass); `demo-broken` exits 4 (P0 critical) for paging-pipeline rehearsal. |
| `frontend/` | **Wave 11+12.** React 18 + Vite 5 + TypeScript 5.6 (strict) SPA. Three panels — Trader (open/close + wallet sign+submit + price hero + position table), IndexerState (sub-pool / dormant / init-hint dashboards), Keeper Console (`KeeperLoopMetrics` + Top-N `RotatePrediction` bars + recent signatures). **Wave 12** introduces `feed/FeedAdapter` interface (`MockFeedAdapter` + `WebSocketFeedAdapter` placeholder + `useFeed(adapter)` hook) and `wallet/WalletAdapter` interface (`MockWalletAdapter` + `WindowWalletAdapter` placeholder detecting `window.solana.{isPhantom,isBackpack,isSolflare}`). `?feed=live` URL param swaps to the websocket adapter. `TraderPanel` now actually invokes `wallet.signAndSubmit(...)` for opens/closes — wave 13 will replace the synthetic signatures with real ones. |
| `programs/mole-option/` | Solana / Anchor on-chain program (wraps `clearing-core` + `pyth-adapter`) |
| `SECURITY.md` | **Wave 12.** Threat model (A-1..A-7 adversaries) + 27-invariant catalogue (CORE / ONCH / GOVN / KEEP / OPS prefixes, each pinned to a specific test function) + trust assumptions + vulnerability disclosure SLA + bug bounty tiers + audit firm onboarding package. |

## Why this layout

Solana smart contracts are immutable once deployed. The hardest, riskiest pieces of MoleOption are:

1. **The math** — fixed-point, checked, no silent dust loss.
2. **The clearing engine** — O(1) shares model with `sync_pool`, dormant buckets, lazy migration.
3. **Equivalence with the whitepaper** — the production engine must agree, in aggregate, with the per-position model.

We isolate all three into pure-Rust crates that run on the host. They are exhaustively unit-tested **and** property-tested with random walks of opens, closes, syncs, rotates, and recovery claims. Only after these crates are bullet-proof do we bridge them into the Solana program; the program then becomes a thin wrapper that does account validation and SPL transfers but never re-implements the math.

## Status

**Wave 29 — protocol rollup wired end-to-end + go-live checklist (`Docs/Planning/26`). The prober daemon now emits a `protocol` block in its JSON snapshot + `mole_prober_markets` Prometheus gauges; the Overview landing page consumes them to show a protocol-health banner above the live TVL/OI KPIs. (Wave 28 shipped the rollup + Overview page as code; wave 29 makes them live.)**

**465 Rust tests host-only · 154 frontend vitest tests · 0 failures · clippy clean (`-D warnings`, default + `solana-rpc` features) · `npm run typecheck` clean (strict + `exactOptionalPropertyTypes` + `noUncheckedIndexedAccess` + `verbatimModuleSyntax`) · `npm run build` clean.**

Wave 21 closes the four production-readiness gaps wave 20 left at the trait boundary:

1. **`keeper_decoder::OnchainPosition` + TS `decodeOnchainPosition`.** Byte-for-byte mirror of the on-chain `Position` PDA (247 bytes = 8 disc + 239 body). Schema field count 80 → **103**; `SCHEMA-MAPPING.md` + `verify-schema-parity.sh` lock all 103 rows. Unlocks wave-22 `websocketAdapter` position decode so `PositionSummary.marketPdaHex` comes from chain data, not mock tags. 5 Rust + 6 frontend tests.
2. **`ops-toolkit::solana_rpc::SolanaRpcAccountSource`.** Production `RpcAccountSource` impl behind `solana-rpc` feature; forwards to `keeper-rpc/solana-rpc` so keeper-bot and prober share `solana-client = 4.0`. Three constructors (`new` / `new_with_timeout` / `from_client`); `sleep_ms` wires to real backoff for retry path. Default builds stay Solana-dep-free.
3. **`RpcMarketFetcher` retry + backup RPC.** `retry_attempts` / `retry_backoff_ms` (both default 0 — wave-20 byte-identical). `with_backup(backup_source)` samples backup `getSlot` once per cycle → `RpcFacts.primary_backup_slot_diff` (was hard-coded 0). Backup never reads accounts; backup `Err` collapses to 0. 9 fresh unit tests.
4. **`/metrics-multi` JSON route + `KeeperMetrics::render_json_snapshot`.** Stable camelCase JSON per market via `MarketRegistry::render_per_market_json`; new `spawn_metrics_server_with_multi` API; wave-12 `/metrics` unchanged (404 when no multi provider). 7 fresh keeper-bot tests.

**Wave 20 — multi-market position filter + live RPC `MarketFetcher` abstraction + `--markets-stdin` / `--env-from-file=PATH` SOPS pipeline.**

**417 Rust tests · 108 frontend vitest tests · 33 ops-toolkit/ts vitest tests.**

Wave 20 closes the trader-side multi-market gap, lifts the prober daemon from a fixture-only scaffold to a real on-chain probe, and threads SOPS-encrypted configs straight through the binary without ever touching disk:

1. **Per-market position filter on the trader panel.** `PositionSummary.marketPdaHex?` tags each position with its owning market PDA; `selectActiveMarketSnapshot` filters `feed.positions` via the new pure helper `filterPositionsByMarket`. Untagged positions stay (back-compat with wave-9..18 single-market mocks); tagged positions for *other* markets are dropped. Mock generator wires the tag for free.
2. **Per-market keeper view.** `MarketViewEntry.keeperState?: KeeperState` carries the active market's keeper-bot metrics when the multi-market run loop publishes them; the snapshot rewriter swaps `feed.keeper` with a paused-flip overlay so `KeeperPanel` stops showing globally-averaged numbers. Falls back to the global mirror when per-market metrics are absent — wave-21's keeper-bot publish change becomes a transparent cut-over.
3. **`ops-toolkit::rpc_fetcher` — `RpcMarketFetcher` for the prober.** New `RpcAccountSource` trait abstracts `solana-client::RpcClient::get_multiple_accounts` + `getSlot`; `RpcMarketFetcher` bulk-fetches Market PDA + KeeperLeaderLock PDA in **one** `getMultipleAccounts` round-trip per market per cycle, decodes via `keeper_decoder` (Anchor-discriminator-prefixed `OnchainMarket` / `KeeperLeaderLock`), and assembles a complete `HealthContext` (paused / frozen / schema_version / leader_lock with cluster slot from `getSlot`, latency recorded into `RpcFacts.primary_get_slot_p95_ms`). 10 fresh unit tests hit decode happy paths, missing PDAs, transport failures on both calls, schema mismatch propagation, and config defaults — all sandbox-internal via `StubRpc`.
4. **`ops-toolkit::cli_loader` — `--markets-stdin` and `--env-from-file=PATH`.** New host-testable CLI helper provides `MarketsSource` (`File` | `Stdin`), `EnvSource` (`Process` | `File` | `Inline`), `parse_env_file`, `extract_sources`, `load_registry`, `read_process_stdin`. Both `prober` and `scan` subcommands pick up the flags. SOPS one-liner: `sops -d markets.enc.toml | ops-toolkit prober --markets-stdin --env-from-file=/run/secrets/prober.env /var/lib/.../mole.prom /var/lib/.../prober.json 10 0`. `KEY=VALUE` overlay files support `export ` prefixes, optional double-quote stripping, comments, and blank lines; missing keys fall back to the live process env.

**Wave 19 — multi-market user product + `ops-toolkit prober` daemon + `${VAR}` env-var-driven configs.**

Wave 19 turned the wave-18 multi-market **infrastructure** into operator-visible **product**:

1. **Multi-market user panels.** New `MarketSelector` pill row sits between `LeaderLockGrid` and the panel tabs; clicking switches the active market. Selection persists via URL `?market=` query string + `localStorage["mole.activeMarket"]`. Panels themselves are unchanged — `selectActiveMarketSnapshot` rewrites `feed.indexer / feed.keeper` to the active market's decoded view, so `TraderPanel` / `IndexerPanel` / `KeeperPanel` consume `feed.indexer` exactly as they did in wave 14.
2. **End-to-end multi-market decoding.** `MultiMarketFeedAdapter` now subscribes (when the caller supplies discriminators) to a single shared `onProgramAccountChange(programId)` stream and routes sub-pool / dormant-bucket updates to the owning market by inspecting `OnchainSubPool.market` / parent sub-pool pubkey. `MarketViewEntry` carries fully decoded `marketSummary` / `subPools` / `dormantBuckets` / `projectedRecoveryOutstandingMicroUsdc` / `indexerSlot` per market.
3. **`ops-toolkit prober` daemon.** Trait-driven loop (`MarketFetcher` + `ProberClock` + `ProberSink`) keeps the test path zero-dep and synchronous. Each cycle calls `scan_all_markets`, renders a unified Prometheus textfile (every metric line auto-relabelled with `market="<symbol>"`), publishes a stable JSON snapshot, and propagates the worst exit code. Strict fail-closed on fetcher errors so AlertManager's `for: 30s` rule trips on a textfile gap rather than stale all-Pass data.
4. **`markets.toml` `${VAR}` substitution.** New `MarketRegistry::from_toml_str_with_env` expands `${VAR}` placeholders and the `$$` escape against a caller-supplied lookup before TOML parsing; the default reads `std::env::var`. Mirrored byte-for-byte in `ops-toolkit/ts/lib.ts::substituteEnvVars`. SOPS workflow: `eval "$(sops -d secrets.enc.env | sed 's/^/export /')" && ops-toolkit prober …` — secrets never land on disk in plaintext.

**Wave 18 — multi-market native: `MarketRegistry` + multi-market keeper-bot run loop + frontend `LeaderLockGrid` + ops-toolkit multi-market scan + `markets.toml` shared schema.**

Wave 18 turned multi-market support into a first-class product feature instead of a "single market for now" approximation:

1. **Shared multi-market schema.** New `keeper_rpc::MarketRegistry` parses `markets.toml` (deliberately tiny `[[markets]]` subset, hand-written 100-LoC parser + base58 decoder, **0 new transitive deps**) into a vector of `{symbol, program_id, market_pda, lock_pda, expected_leader}` entries. The same logical schema is mirrored on the frontend (`VITE_MARKETS` JSON env) and in the TS CLI (`parseMarketsToml` byte-aligned with the Rust subset) so ops can ship one config consumed by all three sides.
2. **Multi-market keeper-bot runtime.** `keeper_bot::MarketRegistry::from_config_with` bridges the config-side registry into a runtime fleet of `MarketSlot`s; the existing `run_loop_multi_market_leader_and_rpc_reconcile` ticks every market in turn with per-market reconcile / heartbeat / wave-17 graceful release. Two end-to-end multi-market tests verify "two markets, distinct outcomes" and "shutdown releases only the leader slots".
3. **Frontend `LeaderLockGrid`.** New `MultiMarketFeedAdapter` subscribes to N market PDAs + N lock PDAs + a shared `getSlot` poll; `FeedSnapshot.marketsView.entries` carries one row per market. New `LeaderLockGrid` renders `symbol × status × holder × slots` with inline `expected_leader` mismatch badges; `App.tsx` auto-switches between the wave-16 single-market banner and the wave-18 grid.
4. **Ops-toolkit multi-market scan.** `ops_toolkit::scan_all_markets` fans the wave-12..17 21-check battery across every registered market and auto-injects `expected_leader` into `LeaderLockFacts`. New `ops-toolkit scan ./markets.toml` CLI mode + new `ops-toolkit/ts/keeper-leader-show-all.ts` ("KL-09") give ops both a Rust prober entry point and a one-shot human-readable dump.

**Wave 17 — frontend live `KeeperLeaderLock` data, keeper-bot graceful release, ops-toolkit health checks + TS CLI scripts, manual ops console, CI SBF stub.**

Wave 17 closes the five wave-16 sandbox-internal pending items in one shot:

1. **Live `LeaderLockBanner` data.** `WebSocketFeedAdapter` now subscribes to the `KeeperLeaderLock` PDA via `accountSubscribe` and (optionally) polls `getSlot()`; raw bytes flow through `FeedSnapshot.keeperLeaderLockBytes` and `currentSlot`, `App.tsx` decodes via wave-15 wasm, and the wave-16 `view={null}` placeholder is gone.
2. **Graceful keeper-bot shutdown.** New `try_graceful_release` + `LeaderRpcReconcileConfig.release_on_shutdown=true` make the wave-16 run loop publish a `keeper_leader_release` ix on SIGTERM/SIGINT when the bot was the leader. Maintenance gap drops from `≈30 s` (wave-15 takeover threshold) to `≤ 16 s` (standby reconcile cadence), often `< 5 s`.
3. **Ops-toolkit Rust 3 new keeper-leader checks.** `keeper_leader_lock_initialized` (P1), `keeper_leader_lock_freshness` (P1, 60/90 % tier), `keeper_leader_lock_holder_matches_expected` (P2). Default opt-in via `HealthContext.leader_lock = None` so single-replica probers stay quiet.
4. **`ops-toolkit/ts/` 5 CLI scripts.** `init / show / acquire / heartbeat / release` now exist as real, runnable, vitest-pinned TS scripts referenced by the wave-16 runbook. Zero Anchor IDL dependency — discriminators recomputed from `sha256` to lock-step with `keeper-decoder::ix`. `KeeperPanel.LeaderLockOpsCard` is the browser equivalent (3 wallet-driven buttons).
5. **CI SBF job stub.** `.github/workflows/ci.yml` adds an `ops-toolkit-ts` job (typecheck + vitest) and a gated `solana-program-test` job (`runs-on: [self-hosted, solana-sbf]` + `if: false`); flipping the gate runs the wave-16 reject-matrix harness.

**Wave 16 — single-snapshot tick + on-chain `KeeperLeaderLock` reconcile + real heartbeat publish + frontend leader banner + SBF reject matrix skeleton.**

Wave 16 closes the wave-15 production gaps in three places:

1. **Single-snapshot tick.** New `KeeperBot::tick_with_snap(&snap, …)` extracts wave-9 / wave-10's post-refresh tick pipeline (vol → predictor → scheduler → executor) into a snapshot-only function; the leader-gated run loops (`run_loop_with_leader` and the new `run_loop_with_leader_and_rpc_reconcile`) now refresh once per tick instead of twice. The original `KeeperBot::tick(fetcher, …)` is preserved as a thin wrapper, so every existing caller is byte-identical.
2. **On-chain reconcile + heartbeat publish.** `crates/keeper-rpc/src/leader_tx.rs` adds `LeaderInstruction` + three `build_keeper_leader_{heartbeat,acquire,release}` builders (re-using the wave-15 byte-exact encoders), a `KeeperLeaderTxBuilder` trait + `MockKeeperLeaderTxBuilder` for host tests, a `SolanaTxBuilder` real-RPC impl under `--features solana-rpc`, and `fetch_keeper_leader_lock(fetcher, lock_pda)` that decodes the on-chain PDA via any `AccountFetcher`. `run_loop_with_leader_and_rpc_reconcile` ties them together: every 20 ticks reconcile the host mirror against chain state, every 5 ticks (and immediately on becoming leader) publish a heartbeat ix; transient RPC failures fall back to the cached mirror without breaking the loop.
3. **Operator-facing leader banner.** `frontend/src/panels/LeaderLockBanner.tsx` consumes wave-15's `decodeKeeperLeaderLockBytes` and renders a colour-coded status bar (`uninitialised` / `unowned` / `fresh` / `stale`) above every panel — the four states cover the full SOP matrix in `Docs/Planning/24-operator-runbook.md § 6.5`. `programs/mole-option/tests/keeper_leader.rs` lays out a `solana-program-test` reject matrix (5 cases + CU budgets) gated behind the `_keeper_leader_program_test` feature so the SBF runner can flip a single feature flag to run it.

**Wave 16 baseline (continued — wave-15 entries kept for context).**

| Component | Status |
|-----------|--------|
| `molemath` | ✅ 17 tests (incl. 4 prop tests) |
| `clearing-core` | ✅ **43 tests** (Wave 8 +16 safety-gates): smoke + conservation prop, event emission, dormant cycles, **wave-8 schema_version × paused × frozen reject matrix with atomic-revert assertions** |
| `simulation` (offline oracle) | ✅ equivalence vs `clearing-core` |
| `indexer` | ✅ unit + cross-model equivalence (4-seed × 50-step random walks where indexer projection ≡ oracle byte-for-byte without rotations) |
| `pyth-adapter` | ✅ 14 tests: happy path + every documented attack vector (wrong owner/magic/version/status/expo, stale, wide conf, non-positive price, undersized account) |
| `protocol-harness` | ✅ **30 tests**: 7 smoke + 1 random-1000-op × 4-seed × 100-trader workload + 3 indexer parity (bound now 1 ppm) + 7 rotation-focused + 9 adversarial + 2 atomic-revert regression + 1 keeper-drain-equivalence. **Found & fixed a real Solana-tx-revert-semantics bug; aggregate chain↔indexer drift dropped from 0.7%–1.5% to < 1 ppm.** |
| `chain-mirror` | ✅ **Wave 5+8+9**, **14 tests**: 4 wave-5 smoke + 3 byte-equal parity properties + 4 wave-8 keeper-init-PDA-lifecycle tests + **3 wave-9 governance tests** (`pause` rejects all 5 funds entrypoints in a single tx; `freeze_new_position` blocks only `open` not `close`; **a premature `bump_market_schema_version` immediately freezes the protocol with `SchemaVersionMismatch` on every funds path** — the keystone defense against admin-multisig compromise). |
| `keeper` | ✅ **Wave 8+9+10**, **35 tests**: 12 wave-8 priority/throttling + 5 wave-9 RotateRiskPredictor + 3 wave-9 ActionExecutor + 3 chain-mirror integration + **8 wave-10 RealizedVolatilityEstimator** (time-weighted σ̂; clamp + warm-up + reset + age/count eviction) + **6 wave-10 KeeperLoop** (idle / explicit hint / vol auto-tune warm-up / auto-tune-off pinning / flaky-executor partial-failure metrics / `metrics::merge`). |
| `keeper-rpc` | ✅ **Wave 10**, **21 tests**: 3 borsh account round-trip + discriminator strict; 3 `MockAccountFetcher` (program_accounts memcmp); 2 PDA seed layout pinning; 6 `ChainSnapshot` (refresh / missing accounts / paused / schema mismatch / forensic skip / clear); 7 tx (**`discriminator_constants_match_sha256_of_anchor_namespace` self-test caught all 3 hard-coded discriminators wrong on first commit** — production safety net works); 2 `RpcExecutor` execute paths. |
| `keeper-bot` | ✅ **Wave 10+12**, **27 tests**: 2 lib (default config; health helper) + **5 e2e integration** (idle market → 0 actions; explicit init hint dispatch consistency; paused → bot errors; schema mismatch → bot errors; **vol estimator warm-up across 40 ticks transitions `applied_vol` from None → Some(σ̂)**) + **wave 12 +20 production tests**: 7 `metrics` (counter increments / NaN warming-up / `# HELP`+`# TYPE` line shape / leader-status round-trip / wallet-balance latest-write / Prometheus parser-grammar invariant / snapshot-error counter), 4 `run` (governance permanent vs RPC transient classification, clamp_ms overflow, observation round-trip), 8 `serve` (200/404/405/503/query-strip/end-to-end on `127.0.0.1:0`). |
| `ops-toolkit` | ✅ **Wave 12**, **26 tests**: 12 threshold tests across all 18 checks (global pause / market pause / frozen-new-position / schema match / trading activity / dormant inventory / recovery outstanding / pending init hints / keeper alive / failed-actions rate / skipped-actions rate / vol estimator stuck / wallet balance / RPC primary latency / RPC primary-backup lag / getProgramAccounts latency / oracle slot age / oracle confidence) + 7 report formatting tests (JSON brackets balance, special-char escaping, severity worst-of, exit-code derivation matches P-tier ordering) + 1 `CHECK_NAMES` order alignment + 1 happy-path full-run + 1 `P0 critical` end-to-end exit code. |
| `programs/mole-option` | 🚧 **Wave 7+8+9.** All five dormant-bridge handlers (`sync_pool` / `close / force_close / claim / pre_sync`) rewired through `run_bridged`. `close_dormant_bucket` lets keepers reclaim rent. `init_distribution_ledger` + `init_dormant_bucket` materialise per-direction PDAs. Wave-7.2 dead-PDA bug fix (`record_is_dead` predicate). Wave 8 wires `harvest_dust(market, sub_pool, direction)` onto the pause / schema_version circuit. **Wave 9** lands the full governance + migration instruction set: `unfreeze_new_position` (closes wave-1's one-way freeze), `set_globally_paused` (admin kill switch on `GlobalConfig`), `bump_market_schema_version` (admin, monotonic-only), `migrate_position` (permissionless), `migrate_market` (admin) — all gated by Anchor `address = ...` checks against the right Squads authority on `GlobalConfig`. Build requires Solana / anchor toolchain (wave 10 sandbox stalled twice on platform-tools download; CI playbook ready in `Docs/Planning/20-…md` § 10.6 for wave 11). |
| Frontend / backend | ✅ Indexer drift fixed; can consume `IndexerState::sub_pool_stats / dormant_inventory / projected_recovery_outstanding`; **wave-9 keeper console** can render `keeper::Scheduler::plan(...)` queues *and* `RotateRiskPredictor::populate_scheduler` predictions directly; **wave-10 keeper-bot dashboards** consume `KeeperLoopMetrics` (per-tick actions_{planned, submitted, failed, skipped} + applied_vol + init_hints_recorded). |
| Dormant bucket on-chain | ✅ wave 5 host-side byte-equal-proven; wave 6 `sync_pool` bridge done; wave 7 all five handlers + `close_dormant_bucket` + `keeper_drain_equivalence`; wave 8 chain-mirror strict-PDA-lifecycle mode + `keeper` crate + on-chain-day-one safety net; wave 9 auto-init via `RotateRiskPredictor`; **wave 10** end-to-end runnable bot on `keeper-rpc` + `keeper-bot`. |
| **Wave 5.5 lazy mode conservation** | ✅ `DormantStore::pending_distribution_total` wired end-to-end. Engine bug fix: drain activated buckets first, drop the optimistic non-activated `last_applied_index` bump. |
| **Wave 7 dormant-bridge bug fix** | ✅ `unpack_direction` skips fully-dead PDAs via `record_is_dead`; `pack_direction` Pass 1/2/3 use the same predicate. **Wave 8 promoted** the predicate to `clearing_core::OnChainBucketRecord::is_dead()` — single source of truth shared by `unpack_dormant_store`, the on-chain bridge, and `chain_mirror`. |
| **Wave 8 on-chain-day-one safety net** | ✅ `clearing_core::SCHEMA_VERSION_CURRENT` + `assert_schema_version` enforced at every funds-touching engine entrypoint (and on `position.schema_version` for position-bearing entries) → silent corruption is impossible after a schema bump. Full pause / frozen-new-position circuit audit (`harvest_dust` newly gated by `paused` & schema check). 16 host-side reject single-tests assert atomic revert per gate. |
| **Wave 9 governance + auto-rotation prediction** | ✅ Full governance instruction set on chain (5 new entrypoints, 3-tier authority via `GlobalConfig`); chain-mirror governance setters + 3 reject-matrix tests; `keeper::RotateRiskPredictor` (Abramowitz-Stegun Φ approx, no `libm`) drives `Scheduler::record_init_hint` automatically; `keeper::ActionExecutor` trait + `DryRunExecutor` decouple planner from RPC client (wave-10 plug-in slot). |
| **Wave 10 keeper bot end-to-end closure** | ✅ `keeper::RealizedVolatilityEstimator` (time-weighted σ̂ from oracle prices, auto-feeds `PredictorConfig`) + `KeeperLoop` (synchronous tick state machine, no tokio, host-testable); new `keeper-rpc` crate (borsh account mirrors + `AccountFetcher`/`TxBuilder` traits + `ChainSnapshot` + `RpcExecutor` + Anchor IX discriminator pinning via sha2 self-test); new `keeper-bot` crate (runnable daemon, 5 e2e integration tests cover idle/init/paused/schema-mismatch/vol-warm-up). 216 tests total, clippy clean, sandbox-resilient (no `solana-client` in default features). |

## Building & testing host crates

Requirements: stable Rust ≥ 1.79.

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

All three should be green.

## Building the Solana program

The `programs/mole-option` crate is **excluded** from the workspace because it depends on `anchor-lang` and the SBF toolchain. To build it:

```bash
# 1. Install Solana CLI
sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"

# 2. Install Anchor (avm + 0.31)
cargo install --git https://github.com/coral-xyz/anchor avm --locked
avm install 0.31.0
avm use 0.31.0

# 3. Build
anchor build
```

This produces `target/deploy/mole_option.so`.

## Architecture cheat sheet

```
                ┌───────────────────────────────────────┐
                │ Pyth oracle account (price + conf)    │
                └──────────────┬────────────────────────┘
                               │  raw bytes
                               ▼
                    ┌──────────────────────────┐
                    │ pyth-adapter (pure Rust) │
                    │  validate owner / magic  │
                    │  / version / status /    │
                    │  expo / age / conf / sign│
                    │  → ValidatedPrice@1e8    │
                    └────────────┬─────────────┘
                                 │
                                 ▼
       ┌──────────────────────────────────────────────────┐
       │ programs/mole-option (Anchor)                    │
       │                                                  │
       │   instructions/{open, close, sync, claim, ...}   │
       │   state/{Market, SubPool, Position, Bucket, ...} │
       │                                                  │
       │      ┌──────────────────────────────────┐        │
       │      │ clearing-core (pure Rust)        │        │
       │      │   sync_pool · open · close       │        │
       │      │   dormant store · invariants     │        │
       │      │   EngineEvent emission           │        │
       │      └──────────────────────────────────┘        │
       │                       ▲                          │
       │                       │ calls                    │
       │      ┌────────────────┴─────────────────┐        │
       │      │ molemath (pure Rust)             │        │
       │      │   mul_div_floor / ceil           │        │
       │      │   signed_pnl_increment           │        │
       │      │   price_move_bps                 │        │
       │      └──────────────────────────────────┘        │
       └────────────────┬─────────────────────────────────┘
                        │  EngineEvent stream
                        │
       ┌────────────────▼─────────────────────────────────┐
       │ indexer (off-chain, pure Rust)                   │
       │   replays events → per-position view             │
       │   (locked_loss / realized_profit_balance)        │
       │   front-end source of truth                      │
       └────────────────┬─────────────────────────────────┘
                        │
                        ▼  cross-model equivalence
       ┌──────────────────────────────────────────────────┐
       │ simulation (offline oracle, whitepaper §3)       │
       │   per-position locked_loss / realized_profit     │
       │   pure ground-truth, never deployed              │
       └──────────────────────────────────────────────────┘

       ┌──────────────────────────────────────────────────┐
       │ protocol-harness (host, pure Rust)               │
       │   wraps clearing-core + indexer + simulated      │
       │   SPL vault & fee vault into one state machine.  │
       │   1000-op × 4-seed property tests; adversarial   │
       │   scenarios; check_invariants() after every op.  │
       └────────────────┬─────────────────────────────────┘
                        │  same op stream, byte-equal events
                        ▼
       ┌──────────────────────────────────────────────────┐
       │ chain-mirror (host, pure Rust) — wave 5/5.5      │
       │   replicates Anchor's account-level runtime:     │
       │     SubPool / DormantBucket / Ledger / Position  │
       │     each as an owned struct;                     │
       │   bridges via pack/unpack_dormant_store;         │
       │   emulates Solana tx-revert at every entrypoint. │
       │   3 byte-equal property tests vs harness         │
       │   (Eager / Lazy / Stress; up to 800 ops × 3 sp). │
       │   Wave 5.5: 4-term vault decomposition           │
       │   `vault == pool_eq + dormant_accrued            │
       │            + dormant_pending + dust`             │
       │   verified at every step in BOTH eager & lazy.   │
       └──────────────────────────────────────────────────┘

       ┌──────────────────────────────────────────────────┐
       │ programs/mole-option/instructions/dormant_bridge │
       │   wave 6: same pack/unpack flow as chain-mirror, │
       │   but reading/writing real Anchor account types. │
       │   sync_pool rewritten end-to-end; close /        │
       │   force_close / claim follow same template       │
       │   (wave 6.5). New init instructions:             │
       │     init_distribution_ledger(direction)          │
       │     init_dormant_bucket(direction, tick)         │
       └──────────────────────────────────────────────────┘
```

## Key design decisions (locked)

- **O(1) directional-equity-pool shares model** for current-block instant settlement. See `Docs/Planning/14-当前区块即时清算与简化模型评估.md` and `Docs/Planning/18-shares模型实现细则与边界条件.md`.
- **No `Depleted` status.** Zero-equity positions stay `Open`-with-recovery-shares; they can recover when new counterparty losses arrive.
- **`locked_loss` is monotonic; `realized_profit_balance` recovers.** Both are derived in the indexer; the on-chain `Position` only stores `active_shares` / `recovery_shares`. See spec §7 and the `clearing-core` engine.
- **Dilution safety.** `dilution_safety_bps` triggers an automatic active→recovery rotation when `pool_equity / shares` falls below the safety floor. (Documentation §5.5 had a transposed inequality; both code and doc are now consistent — see `engine.rs::open_position`.)
- **Oracle integration is gated by price protection.** Every funds-sensitive instruction takes `expected_min / expected_max / expected_max_age_slots`.

## End-to-end protocol harness

`crates/protocol-harness` simulates a complete MoleOption market on the host: **multi-sub-pool**, **multi-trader**, an **SPL-vault stand-in**, a **fee vault**, and the **indexer wired in lock-step** with the chain. Every state-changing call is followed by `Harness::check_invariants()`, which enforces:

```
total_deposits        == total_withdrawals + vault + fee_vault
vault                 == Σ pool_equity + Σ dormant_accrued + Σ dormant_pending + Σ dust
fee_vault             == Σ open_fees + Σ swept_dust
```

The fourth term `dormant_pending` is the wave-5.5 lazy-mode in-flight allocation: funds the engine routed out of `pool_equity` via `distribute_lazy` but that no bucket has yet pulled into its `accrued_value`. In eager mode this is always 0; in lazy mode it equals `Σ DormantStore.pending_distribution_total` and the harness verifies it on every step.

**1000-op × 4-seed × 100-trader × 3-sub-pool random workloads pass conservation at every step.** Adversarial scenarios (envelope deviation, force-close-without-acknowledge, cross-sub-pool isolation, oversized price step, paused market, frozen new positions, sub-min-margin opens, force-closing positive-value positions) all reject cleanly.

### Found & fixed: Solana tx-revert semantics bug

The harness uncovered a 0.7–1.5 % aggregate drift between chain `withdrawable` and indexer `equity()` in compound `rotation + multi-position bucket + claim_recovery + new entrants` paths. Root cause was **not** an indexer algorithm bug — `clearing_core::close_position` mutates `sub_pool` in stages (sync → lazy_migrate → burn active → redeem from dormant bucket), then checks `withdrawable == 0` last; on Err the chain state was partially mutated (including bucket deletion) but the accumulated events were dropped, so the indexer never received the burn notifications and held "ghost" buckets. Solana's transaction runtime auto-reverts on Err so this is invisible on-chain; the host-side harness needed to emulate it.

**Fix (wave 4):** every `Harness` entry point now wraps the engine call in a snapshot/restore pair (`let snap = sub_pool.clone(); ...; if Err: *sub_pool = snap`). Aggregate drift collapsed from 0.7–1.5 % to **< 1 ppm** (170 raw units / ~2×10¹⁰ deposits — pure floor-rounding noise). The `aggregate_chain_payouts_match_indexer_with_rotations` test bound was tightened from the wave-3 placeholder of 2 % back to **1 ppm** with a 1024-unit absolute floor. New `tests/atomic_revert.rs` regression suite locks the fix in place. `clearing_core::close_position`'s doc-comment now declares the atomicity contract explicitly. See `Docs/Planning/22-wave3-protocol-harness.md` §4 for the full forensics.

## What is verified today

- `molemath` round-trip identities for `mul_div_floor` / `mul_div_ceil` and PnL anti-symmetry.
- `clearing-core` random-walk conservation: across thousands of randomized open/close/sync sequences (4 prop cases × 50–150 steps), the engine never releases more value than was deposited; rounding always rounds *toward* the protocol (dust accrues, never overdraws).
- `clearing-core` reverse-dilution gating (`DilutionRiskTooHigh`).
- `clearing-core` zero-shares rejection (`SharesMintedTooSmall`).
- `clearing-core` price protection failure & per-sync price-move cap.
- `clearing-core` long/short pool-zeroing rotates active to recovery and seeds a dormant bucket; `force_close_zero_value_position` requires explicit `acknowledge_forfeit`.
- `clearing-core` event emission: every state transition produces a structured `EngineEvent` stream (`PoolSync`, `PositionOpened`, `PositionClosed`, `PositionForceClosed`, `DormantRecoveryClaimed`, `ActiveRotatedToRecovery`, `DustHarvested`).
- `clearing-core` multi-cycle dormant lifecycle: 3 boundary tests cover two coexisting buckets at different anchor prices, two consecutive rotations with stale `active_generation` lazily migrated on next touch, and 3 random-seed oscillation walks where the protocol never over-pays and the accounting gap stays ≤ 4096 raw units.
- `simulation` ↔ `clearing-core` equivalence on the canonical Alice/Bob/Charlie scenarios.
- `indexer` per-position projection: across 4-seed × 50-step random walks of two-trader symmetric scenarios, the indexer reconstruction of `(locked_loss, realized_profit_balance)` from chain events matches the simulation oracle **byte-for-byte**.
- `indexer` heterogeneous-open conservation: with 3 traders at different entry prices, the indexer's view of total equity tracks the chain to ≤16 raw units per position and ≤32 raw units in aggregate.
- `pyth-adapter` rejects every documented Pyth attack vector: wrong account owner, bad magic / version / atype / status, exponent out of `[-18, 0]`, non-positive price, stale-by-N-slots, confidence wider than `max_confidence_bps`, undersized account; happy-path validates and rescales prices with exponents in `{-6, -8, -10}` to `PRICE_SCALE = 1e8` without floating-point.

## Wave 5 dormant bridge & chain-mirror

`programs/mole-option/src/instructions/sync.rs` had a wave-1-to-4-undetectable production blocker:
its `clearing_view(sp_acc)` constructed an empty `DormantStore` on every instruction and discarded
any rotation result on write-back. The host engine and harness were 100 % tested but **the on-chain
instruction wiring of the dormant lifecycle was a no-op** — first rotation in production would
silently lose the new bucket.

`crates/chain-mirror` (wave 5) closes that gap on the host. It models every Anchor `#[account]` as
an owned struct, drives instructions through `clearing_core::pack_dormant_store` /
`unpack_dormant_store`, and emulates Solana's tx-revert semantics on `Err`. It is then driven
side-by-side with `protocol-harness` against the same randomized op streams; the parity property
tests assert **byte-equal** sub-pool / bucket / ledger / position state and identical Ok/Err
classification at every step — across 4400 randomized ops covering eager, lazy, and high-rotation
stress workloads. See `Docs/Planning/23-on-chain-dormant-bridge.md` for the full design and the
per-instruction Anchor account-list contract that wave 6 will implement.

## Wave 5.5: lazy-mode conservation closure + a deeper engine bug fix

Wave 5 left a known follow-up: surface `DormantStore::pending_distribution_total` so the four-term
vault decomposition `vault == pool_equity + dormant_accrued + dormant_pending + dust` holds at every
step in lazy mode. Wiring that field through `pack/unpack_dormant_store`, the on-chain `OnChainLedger`,
the harness, and the chain-mirror was the first half of the work — it surfaced a much deeper engine
bug that wave-1-to-5 unit tests had silently missed.

**The bug.** When the same `DormantStore` saw a mix of lazy and eager `distribute()` calls (which
will happen any time governance flips the market's `dormant_distribute_mode` mid-life, and which the
`onchain_layout::pack_unpack_round_trip_under_random_ops` property test simulates), eager
`distribute()`'s `last_applied_index = event_index + 1` advance for activated buckets was **skipping
past pending lazy entries** for those buckets:

```text
1. distribute_lazy(p1, alloc=10): pending += 10. ledger e0. bucket A.last_applied = 0.
2. distribute(p2, alloc=20)  (eager):
     ▷ reads bucket.accrued = 0 (BUG: never drained pending)
     ▷ allocates share=20 to A. A.accrued = 20.
     ▷ ★ bumps A.last_applied to 2 (event_index+1).
3. apply_pending(A): last_A == next_event ⇒ no-op.
   Result: 10 units from e0 stranded in `pending_distribution_total` forever.
```

`compact_ledger` later drops e0 once every bucket has walked it (which now succeeds — A's
last_applied passed it), but the pending sum is unchanged, so the next pack/unpack trips the
`pending_distribution_total <= Σ entry.allocated_sum_observed` invariant.

**The fix** (`crates/clearing-core/src/dormant.rs::DormantStore::distribute`):

```diff
+    // Drain pending lazy shares for every activated bucket BEFORE
+    // computing this entry's outstanding numerator. In production,
+    // fixed-mode markets never accumulate pending — this loop is a
+    // no-op there. In mixed-mode (governance flip / property tests),
+    // it prevents lost-update.
+    for key in &activated_keys {
+        self.apply_pending_to_bucket(*key)?;
+    }
+
     let mut total_outstanding: u128 = 0;
     for key in &activated_keys { ... }
-    // Buckets that were NOT activated for this event must also have
-    // their `last_applied_index` advanced — the entry is a no-op for
-    // them, but skipping the bump would force them to revisit the
-    // event on every future apply_pending call.
-    for (_, bucket) in self.buckets.iter_mut() {
-        if bucket.last_applied_index <= event_index {
-            bucket.last_applied_index = event_index + 1;
-        }
-    }
```

Removing the bulk-bump for non-activated buckets costs log-length walking on the next
`apply_pending_to_bucket` (already O(stale_entries) bounded by `max_distribution_ledger_size`), in
exchange for making mixed-mode and future mode-flip migrations provably leak-free. Pure-eager and
pure-lazy stores are unaffected.

The harness's bucket-parity check is also tightened in wave 5.5: `indexer.bucket.accrued_value`
must equal `chain.bucket.accrued_value + chain_store.pending_for_bucket(tick)` (the new pure-read
accessor mirrors `apply_pending_to_bucket`'s replay formula byte-for-byte).

## Wave 6: Anchor sync_pool actually goes through the dormant bridge

`programs/mole-option/src/instructions/dormant_bridge.rs` (new) implements the same
`unpack_direction → engine → pack_direction` flow that `chain-mirror::ChainRuntime` runs on the
host, but reading and writing real Anchor account types (`Account<DormantBucket>`,
`Account<DistributionLedger>`). `sync.rs` is rewritten end-to-end to call into it; the new
signature is

```rust
pub fn sync_pool(
    ctx: Context<SyncPool>,
    envelope: PriceEnvelopeArgs,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<()>;
```

`ctx.remaining_accounts` carries `long_bucket_count` long-side `DormantBucket` PDAs followed by
`short_bucket_count` short-side ones. `SyncPool` Accounts struct adds two strict fields:
`long_ledger` / `short_ledger` (one `DistributionLedger` PDA per direction, validated via
`has_one = sub_pool` plus `direction_is_long` constraint).

Two new admin instructions materialise the PDAs the bridge expects:

- `initialize_distribution_ledger(direction_is_long)` — once per `(sub_pool, direction)`.
- `initialize_dormant_bucket(direction_is_long, zero_price_tick)` — keeper / front-end pre-allocates
  these whenever it sees an `ActiveRotatedToRecovery` event suggesting a new tick is about to be
  used.

The `close / force_close / claim` handlers follow the same template (since they internally call
`sync_pool` and therefore need both ledgers + every live bucket); they remain on the wave-5 skeleton
in this wave, slated for wave 6.5. `harvest_dust` does not touch dormant state and is unchanged.

## Wave 7: Bridge consolidation + dead-PDA fix + close_dormant_bucket + lazy-drain ≡ eager proof

Three interlocking pieces close out wave 7.

**1. `run_bridged` helper** — All five dormant-bridge handlers (`sync_pool`, `close_position`,
`force_close_zero_value_position`, `claim_dormant_recovery`, `pre_sync_dormant_bucket`) used to
duplicate ~50 lines each of unpack-engine-pack-exit boilerplate. Wave 7 collapses that into a
single closure-driven helper in `programs/mole-option/src/instructions/dormant_bridge.rs`:

```rust
run_bridged(sub_pool, long_ledger, short_ledger, market_params, program_id,
            remaining_accounts, long_bucket_count, short_bucket_count,
            |params, sp| clearing_core::xxx(params, sp, …).map_err(map_err))
```

Future bridge changes (pre-pack invariants, security checks, error injection, CU optimisation) now
land in exactly one place. host-side `cargo clippy -- -D warnings` clean.

**2. Critical bridge-layer bug fix.** `unpack_direction` previously decoded every bucket account
into the engine's `DormantStore`, including PDAs that a keeper had pre-init'd with all observable
fields zero. Once such a "dead" record was inside the engine, `insert_or_merge(tick, anchor_price,
…)` saw `contains_key(tick)` true and went down the already-exists branch — which **never updates
`anchor_price`**. Result: the bucket sat permanently at `anchor_price = 0`, never activated, never
collected lazy-distribute shares, and accumulated dust-conservation drift. Fix: a new
`record_is_dead(b: &DormantBucket) -> bool` predicate; `unpack_direction` skips dead records;
`pack_direction` Pass 1/2/3 use the same predicate so the bridge's dead-slot semantics align
byte-for-byte at both ends. The `keeper_drain_equivalence` property test (below) guards the fix
end-to-end. Why this didn't surface in chain-mirror parity: chain-mirror's bucket records all come
from the engine via `commit_core_sub_pool`, so it never models the keeper-init-empty-PDA
workflow. Adding a synthetic chain-mirror case for the keeper-init-empty-PDA path is on the
wave-8 backlog.

**3. `close_dormant_bucket` instruction.** Long-running lazy-mode keeper networks accumulate
zero'd-out `DormantBucket` PDAs; without a close path the per-sub-pool PDA pool grows
indefinitely. Wave 7 ships a permissionless `close_dormant_bucket` instruction guarded by two
constraints: `record_is_dead(bucket)` (no live shares / accrued / positions) AND
`bucket.last_applied_index >= ledger.next_event_index` (no pending lazy entries still owed to
this tick). New errors: `DormantBucketStillLive`, `DormantBucketHasPendingApply`. Anchor's
`close = receiver` directive atomically transfers rent and marks the buffer closed.

**4. `keeper_drain_equivalence` property test.** Two `Harness` instances run the same 1200-step
random op stream — one configured `Eager`, one configured `Lazy` plus a `drain_all_buckets` call
between every operation. Snapshots are byte-equal across `(vault_balance, fee_vault,
total_deposits, total_withdrawals, sub-pool scalars, every bucket's anchor_price /
total_recovery_shares / total_recovery_notional / accrued_value / position_count, every position
field)`. The single legitimate difference (`bucket.last_applied_index`) is explicitly excluded.
This is the strongest evidence that lazy mode + keeper drain ≡ eager mode at the per-bucket
level — any future drift in `apply_pending_to_bucket` / `distribute()` / ledger replay math will
trip this test before Solana sees the change. Run via `cargo test --test
keeper_drain_equivalence`.

## Wave 8: on-chain-day-one safety net + keeper software kernel

Wave 8 hardens everything that must hold *the day mainnet ships*. No production protocol gets the
schema-upgrade path right by accident, no lazy-mode protocol survives without keepers, and no
emergency circuit-breaker is trustworthy without a reject-matrix proof. Wave 8 closes all three.

**1. chain-mirror strict PDA lifecycle (Wave 7.2 regression guard).** `ChainRuntime::with_strict_pda_lifecycle(true)` mirrors anchor's `pack_direction` Pass 1/2/3 byte-for-byte. Engine producing a record at a tick whose PDA wasn't pre-init'd → `MirrorError::BucketSlotExhausted` + atomic sub-pool revert (1:1 with on-chain `DormantBridgeBucketSlotExhausted`). `OnChainBucketRecord::is_dead()` + `OnChainBucketRecord::dead(direction, tick)` promoted to *single source of truth* for dead-PDA detection — `unpack_dormant_store` skips dead records directly, the on-chain bridge `record_is_dead` is now a thin wrapper. Four chain-mirror tests permanently lock the wave-7.2 fix: rotate-without-preinit must err; keeper-init + retry must promote `anchor_price` (was 0 before the fix); close-after-drain reclaims rent; close on still-live PDA rejected.

**2. `schema_version` end-to-end.** `clearing_core::SCHEMA_VERSION_CURRENT: u16 = 1` is the single epoch number. `assert_schema_version(found)` runs at the head of every funds-touching engine entrypoint:

| Entrypoint | Gates |
| --- | --- |
| `sync_pool`, `pre_sync_dormant_bucket`, `open_position` | `market.schema_version` |
| `close_position`, `force_close_zero_value_position`, `claim_dormant_recovery` | `market.schema_version` AND `position.schema_version == market.schema_version` |
| `harvest_dust` | `market.schema_version` (Wave 8 newly takes `&MarketParams`) |

Mismatch → `ClearingError::SchemaVersionMismatch` → `ProgramError::SchemaVersionMismatch` on chain → atomic sub-pool revert. Combined with the planned wave-9 Squads multisig + `migrate_position` / `migrate_market` handlers, this is the protocol's full upgrade safety net.

**3. Pause / frozen-new-position circuit audit.** Wave 8 also discovered & sealed a gap: `harvest_dust` was bypassing `paused`, breaking the "pause means everything stops" invariant. Now reflexive: `paused = true` rejects all 7 funds-touching entrypoints; `frozen_new_position = true` rejects only `open_position` (so users can still drain). 16 host-side single-tests in `crates/clearing-core/tests/safety_gates.rs` assert reject + atomic revert per gate via a `SubPoolFingerprint`.

**4. `crates/keeper/` — pure-Rust scheduler kernel.** New crate (145 lines core, 14 tests) closes the lazy-mode operational loop:

```rust
pub trait KeeperChainView {
    fn sub_pool_ids(&self) -> Vec<u32>;
    fn buckets(&self, sub_pool_id: u32) -> Vec<BucketSnapshot>;
    fn ledger(&self, sub_pool_id: u32, direction: Direction) -> Option<LedgerSnapshot>;
}
let actions: Vec<KeeperAction> = scheduler.plan(&view)?;
```

`Scheduler::plan` enumerates every actionable PDA from a chain-side snapshot, priority-ordered:

- `InitDormantBucket { rationale }` — priority 1e9 (missing PDA blocks user txns); v1 emits only after `Scheduler::record_init_hint`.
- `PreSyncDormantBucket { pending }` — priority `1e7 + pending` (more behind = more urgent); fires when `bucket.last_applied_index < ledger.next_event_index`.
- `CloseDormantBucket` — priority 1 (best-effort rent recovery); fires when `bucket.is_dead() && last_applied >= ledger.head`.

Throttling via `SchedulerConfig` (`min_pending_for_pre_sync`, `max_actions_per_plan`,
`close_dead_buckets`). Off-chain reconstruction bug detection
(`KeeperError::BucketAheadOfLedger`). The integration test
`tests/chain_mirror_integration.rs` runs the full lazy loop end-to-end against `chain-mirror`:
open + crash → plan emits PreSync → keeper apply → plan empty → claim → bucket dead → plan
never emits PreSync against the drained bucket. Production keeper bot = `KeeperChainView` over
RPC + `KeeperAction` → Anchor TX serialiser.

## Wave 9 (delivered)

Three pieces shipped on-chain governance, automated rotate-risk hinting, and the executor seam for the wave-10 RPC keeper. See `Docs/Planning/20-攻坚开发进度与里程碑.md` §第九波 + `Docs/Planning/23-on-chain-dormant-bridge.md` §11.

**1. Full governance + migration instruction set.** `programs/mole-option/src/instructions/admin.rs` is now a 3-tier authority hub (emergency / admin-per-market / admin-global) gated by Anchor `address = ...` against `GlobalConfig`. Five new instructions: `unfreeze_new_position` (closes wave-1's one-way freeze), `set_globally_paused` (single-tx kill across every market under one `GlobalConfig`), `bump_market_schema_version` (admin, **monotonic-only** — non-monotonic bumps reject as `SchemaBumpMustIncrease`, so even a compromised admin multisig cannot rewind to a vulnerable schema), `migrate_position` (permissionless walk forward), `migrate_market` (admin, sequenced before `bump_market_schema_version` under the same governance proposal). `programs/mole-option/src/instructions/migration.rs` carries a `SchemaMigrationStep` enum that v1 ships empty — the loop, error codes, and account contexts are already in place so future v2 deployments only add `+ V1ToV2`.

**2. `keeper::RotateRiskPredictor`.** Closes the loop on the wave-8 `InitRationale::RotateRiskHorizon` reservation. Reverse-engineers per-direction zero-equity prices from `SubPoolHealth`, applies a one-touch upper-bound under geometric Brownian motion (`Φ(log(S_zero/S_now)/σ√T)`), and pushes any prediction whose probability beats `min_probability` into `Scheduler::record_init_hint`. Φ is Abramowitz-Stegun rational approximation (max abs err ≈ 7.5e-8), no `libm` dependency — host vs BPF compilation is bit-for-bit deterministic. End-to-end test against chain-mirror: 1% adverse move on a populated sub-pool surfaces a long-zero tick, the scheduler emits an `InitDormantBucket` action, and `pre_init_dormant_bucket` materialises the dead PDA.

**3. `keeper::ActionExecutor` + `DryRunExecutor`.** Trait-based executor seam decouples the read-only `Scheduler` planner from any specific RPC client. `run_plan_cycle(scheduler, view, executor)` returns `Vec<(Action, Result)>` so per-action failures don't abort the cycle — caller decides retry semantics. Wave-10 production keeper bot is now ~200 lines: bind `solana-client::nonblocking::rpc_client` to `KeeperChainView`, translate `KeeperAction` → Anchor IX, install in `ActionExecutor::execute`. `chain-mirror::ChainRuntime` wires up the new `KeeperChainView::sub_pool_health` default-method override so the predictor can run against host-side replicated state.

**3 chain-mirror governance tests** (`governance_pause_immediately_rejects_every_funds_path`, `governance_freeze_blocks_only_open_not_close`, **`governance_bump_without_program_upgrade_freezes_protocol`**) lock the keystone safety guarantee: an admin multisig that bumps `market.schema_version` ahead of the deployed BPF will immediately freeze every funds-touching entrypoint with `SchemaVersionMismatch`. `SCHEMA_VERSION_CURRENT` is a deploy-time constant — no Squads transaction can flip it.

## Wave 10 (delivered)

Wave 10 built the **runnable keeper bot** end-to-end: realised-vol self-tuning, a synchronous tick state machine, and a host-only Solana RPC adapter that's ready to swap to the real `solana-client` behind a feature flag in wave 11. See `Docs/Planning/20-攻坚开发进度与里程碑.md` §10 + `Docs/Planning/23-on-chain-dormant-bridge.md` §12 for the deep dive.

**1. Realised-volatility auto-tuning closes the wave-9 σ-loop.** `keeper::RealizedVolatilityEstimator` rolls a time-weighted σ̂² = Σr²ᵢ / ΣΔtᵢ from `(price, slot)` samples — the simpler stddev(r)·√(N/T) form is unsafe under Solana's ±20% slot-rate jitter. Window is bounded by both `max_samples` and `max_age_slots` (whichever is more restrictive evicts), and σ̂ is clamped to `[0.05, 5.0]` so a transient outlier window can't drive the predictor to σ ≈ 0 or σ → ∞. `apply_to_predictor_config(predictor)` over-writes `predictor.annual_vol` only after the estimator clears `min_samples`, so the caller's hand-tuned default survives the boot window. 8 unit tests cover warm-up gating, constant-price floor clamp, synthetic-walk magnitude recovery, out-of-order/duplicate slot rejection, dual count + age eviction, warm-only application, and reset.

**2. `keeper::KeeperLoop` is the synchronous, tokio-free tick state machine.** One `tick(env: &mut KeeperBotEnvironment)` call: record fresh price sample → apply σ̂ to predictor → predictor populates scheduler init hints → `Scheduler::plan` → dispatch each action through the executor. Per-action failures are captured in `KeeperLoopMetrics::actions_{submitted, failed, skipped}` (no abort), so dashboards can plot the partial-failure rate directly. Same code drives CI integration tests, the offline backtest replay, and the production bot — only the `KeeperBotEnvironment` impl differs. 6 unit tests pin the contract: idle market → zero metrics, explicit init hint → exactly one InitDormantBucket dispatch, `auto_tune_vol=true` warm-up over-writes predictor, `auto_tune_vol=false` pins predictor even when estimator is warm, flaky executor partial-failure metrics line up, `KeeperLoopMetrics::merge` is field-wise.

**3. `keeper-rpc` crate is the host-only Solana RPC adapter.** `default = []` — no `solana-client`, no agave dependency tree on every CI run; the production binding lives behind a `solana-rpc` feature flag for wave 11. Module breakdown: `accounts` (borsh `Onchain{Market,SubPool,DormantBucket,DistributionLedger}` mirrors with byte-aligned Pad) · `pda` (5 seed builders pinned by `seeds_layout_pinned`) · `fetcher` (`AccountFetcher` trait + `MockAccountFetcher` with memcmp `program_accounts` filter) · `snapshot` (`ChainSnapshot::refresh(...)` implements `KeeperChainView`; `SnapshotConfig::{enforce_schema_version, bail_when_paused}` re-uses wave-9 lockdown semantics) · `tx` (`TxBuilder` trait + `RpcExecutor` implementing `ActionExecutor`; `KeeperAction` → `[disc8] ++ borsh(args)` on the wire). **Anchor instruction discriminators are pinned by `discriminator_constants_match_sha256_of_anchor_namespace`** — a `sha2`-based self-test that recomputes `sha256("global:<name>")[..8]` every CI run and compares to the hard-coded `pub const`s. The first-pass commit had **all three** discriminators wrong; the self-test caught every byte mismatch on the very first run, which is exactly the silent bit-rot it's designed to prevent. 21 unit tests across borsh round-trip, fetcher filter, PDA layout, snapshot refresh + missing-accounts/paused/schema/clear paths, IX byte-layout, and `RpcExecutor` execute path.

**4. `keeper-bot` crate is the runnable daemon.** `KeeperBot::tick(fetcher, ctx, builder, keeper_pk, clock_sysvar, system_program)` materialises the full `ChainSnapshot` → vol record → predictor → plan → dispatch loop; `cargo run -p keeper-bot` is the offline smoke runner that exits 0 with friendly logs on an empty fixture. 5 end-to-end integration tests in `tests/end_to_end.rs` are the real wave-10 acceptance gate: idle market emits zero actions; an explicit init-hint path stays internally consistent (`actions_planned == dispatched.len() == submitted.len()`); a paused market short-circuits the bot with `BotError::Snapshot(MarketPaused)`; schema-version drift between on-chain `Market` and the keeper's compiled-in `SCHEMA_VERSION_CURRENT` errors out (the wave-9 lockdown semantics propagate end-to-end); **vol estimator warm-up across 40 ticks transitions `applied_vol` from `None` (cold) to `Some(σ̂)` (warm)** — this is the single end-to-end seam that proves the wave-10 self-tuning loop closes on real chain state.

**5. Solana toolchain retry — sandbox-blocked, CI playbook ready.** Second curl of `platform-tools-osx-aarch64` v1.52 ran for 5m20s and hit `curl: (18) Transferred a partial file` at 324M/395M (~82%, ~1 MB/s avg). Same physical-bandwidth wall as wave 9. The CI runner playbook is now baked into `Docs/Planning/20-攻坚开发进度与里程碑.md` §10.6 with `actions/cache` config, platform-tools symlink, and `cargo build-sbf` sequence — wave 11 picks it up on a real CI runner with stable bandwidth.

## Wave 11 (delivered)

Wave 11 took the host-only `keeper-rpc` adapter live: a `solana-rpc` feature flag binds `solana-client 4.0` to the same `AccountFetcher` / `TxBuilder` traits the host tests use. Because `solana-sdk 4.0` re-exports `solana-transaction 4.0` while `solana-client 4.0` pulls `solana-transaction 3.1`, the adapter explicitly depends on the granular crates (`solana-pubkey`, `solana-transaction`, `solana-instruction`, `solana-commitment-config`, `solana-keypair`, `solana-signer`, `solana-hash`) so the dependency tree is single-versioned. `SolanaRpcAccountFetcher::fetch_program_accounts_filter` uses the raw `client.send(RpcRequest::GetProgramAccounts, ...)` path because the synchronous `RpcClient` in 4.0 doesn't expose `get_program_accounts_with_config` — the 11 unit tests cover Pubkey32 ↔ `solana_pubkey::Pubkey` byte-equal round-trip (this is **KEEP-6** — every off-chain PDA points at the on-chain account), `AccountMeta` flag preservation (**KEEP-7**), `DispatchedAction → Instruction` data-blob byte equality (**KEEP-8**), and unreachable-RPC `Transport`-error classification (**KEEP-9**). 32 keeper-rpc tests pass with `--features solana-rpc`.

Wave 11 also produced `Docs/Planning/24-operator-runbook.md` (1+2-day audit-firm onboarding, IR-01..05 incident playbooks, alert thresholds, "don't do this" list) and the `frontend/` MVP — React + Vite + TypeScript with three panels (Trader / Indexer / Keeper Console), a deterministic mock feed simulating evolving on-chain state, and a dark console-feel UI. Frontend `npm run typecheck` + `npm run build` clean, gzipped 51.88 KB.

## Wave 12 (delivered)

Wave 12 produced **production daemon + ops automation + audit readiness**. The keeper bot grew a Prometheus exporter (13 atomic, lock-free metrics rendered to OpenMetrics 0.0.4 text), a hand-rolled HTTP/1.1 listener for `/metrics` and `/healthz` (no axum / hyper / tokio dependency), and a permanent-vs-transient error classifier (`is_transient` — `MarketPaused / SchemaVersionMismatch / MarketNotFound / SubPoolNotFound / Decode` are permanent and exit; `Rpc(Transport)` is transient and backs off).

`crates/ops-toolkit` automates the 18-row daily-health dashboard from `Docs/Planning/24-operator-runbook.md §2` — pure-function checks, three reporters (human / JSON / Prometheus textfile), severity-keyed exit codes (P0 critical → 4, …). 26 unit tests cover every threshold edge.

The frontend split into modular `feed/` and `wallet/` adapters: `MockFeedAdapter` / `WebSocketFeedAdapter` (placeholder) and `MockWalletAdapter` / `WindowWalletAdapter` (Phantom / Backpack / Solflare detection). `?feed=live` URL parameter switches adapters without changing panel code.

`SECURITY.md` shipped at the root: A-1..A-7 threat model, 27 invariants with test references, 5 trust assumptions, and bug-bounty / disclosure policy.

Total tests **262 / 262 pass** (273 with `--features solana-rpc`). `clippy -D warnings` clean across the workspace.

## Wave 13 (delivered)

Wave 13 is **audit-readiness governance**: take wave 12's audit-readiness from "published doc" to "CI-enforced contract".

**1. CI infrastructure.** New `.github/workflows/ci.yml` runs four parallel jobs (rust / governance / frontend / audit-readiness summary), with `actions/cache` on cargo and npm. The rust job covers `cargo fmt`, `cargo build/test/clippy --workspace --all-targets`, the same with `--features solana-rpc`, plus an `ops-toolkit demo human` (exit 0) / `demo-broken human` (exit 4) smoke pair and a one-tick `keeper-bot serve` daemon smoke.

**2. Three governance verifier scripts.** All bash, all macOS-bash-3.2 compatible, all CI-runnable in seconds:

- `scripts/verify-security-references.sh` — every backticked test reference in `SECURITY.md` (32 of them) is grepped for `fn <name>` in the cited file. Renaming a test fn that an invariant cites trips this on the next PR.
- `scripts/verify-test-counts.sh` — parses the declared "262 / 262 pass" from `Docs/Planning/20-…md`, runs `cargo test --workspace --all-targets`, asserts `passed >= declared` and `failed == 0`.
- `scripts/verify-schema-parity.sh` — parses every `pub <field>:` in the `Onchain*` Borsh mirrors of `crates/keeper-rpc/src/accounts.rs` (80 fields across 5 structs), asserts each is mentioned somewhere in `Docs/SCHEMA-MAPPING.md`. Adding a new Rust schema field without an entry in the mapping doc trips this on the next PR.

**3. `SECURITY.md` repaired.** Running the verifier on the wave-12 publication exposed **20 broken references** — wave 12 had used aspirational test names from the original planning docs rather than the names tests actually grew into (`programs/mole-option/src/handlers/*.rs` is really `instructions/*.rs`; `pool_equity_never_underflows` was the planning-doc name, the actual test is `prop_conservation_under_random_walk`; etc). All 20 are now repointed at live tests; verifier reports `All 32 SECURITY.md test references resolve to live symbols ✓`.

**4. `Docs/SCHEMA-MAPPING.md`.** New explicit accounting of every `Onchain*` Borsh field's TypeScript fate. Each row maps the Rust field to a `FeedSnapshot.*` field with rationale, OR explicitly omits it with one of five reasons (`engine internal` / `anchor internal` / `alignment` / `chain-level` / `wave-14 trade-form`). The doc also includes a reverse direction (TS field → Rust source) so an auditor reading top-to-bottom sees both sides of the mapping.

**5. `CHANGELOG.md` + `CONTRIBUTING.md`.** New repo-root files: `CHANGELOG.md` is the wave-1..13 externally-readable history (Added / Fixed / Tests / Notes per wave); `CONTRIBUTING.md` is the operational companion to `SECURITY.md` (5-min first-time setup, the four CI gates and three governance scripts to run locally, **§3 audit-firm two-day onboarding**, trust-boundary DO/DON'T list).

No code change to the engine, on-chain program, keeper, or front-end — wave 13 is governance-only by design. Tests stay at **262 / 262 pass** (273 with `--features solana-rpc`); all three verifier scripts return clean. Solana-toolchain bring-up still blocked by sandbox bandwidth (5th retry attempted, same `curl: (18) Transferred a partial file` wall) — wave 14 is where it lands on a real CI runner.

## Wave 14 (delivered)

Wave 14 is the first round where **frontend + backend really ship together**. We pulled the on-chain Borsh schemas into a dedicated wasm32-buildable crate, wired the frontend up to a real Solana RPC subscription, and wrote the `signAndSendTransaction` path with full error mapping. Rust **274 / 274 pass** (baseline 262 + 12 new keeper-decoder tests); frontend **31 / 31 vitest pass** (12 decoder + 7 ws-adapter + 12 wallet-adapter); typecheck / build / clippy / 3 governance verifiers all clean.

**1. `crates/keeper-decoder` — schema-only crate that builds on `wasm32-unknown-unknown`.** The five `Onchain*` Borsh structs (`OnchainSubPool`, `OnchainDormantBucket`, `OnchainDistEntry`, `OnchainDistributionLedger`, `OnchainMarket`) plus the `decode_anchor_account*` / `encode_anchor_account` helpers move out of `keeper-rpc` into a zero-Solana-dep crate (just `borsh` + `thiserror`). `keeper-rpc::accounts` becomes a thin re-export shim, so external callers (`keeper_rpc::accounts::OnchainSubPool`, `keeper_rpc::Pubkey32`, …) keep their import paths unchanged. CI YAML now runs `rustup target add wasm32-unknown-unknown && cargo build -p keeper-decoder --target wasm32-unknown-unknown --release` so wave 15 can `wasm-pack build` without further refactoring.

**2. TypeScript Borsh decoder + 12 vitest unit tests.** `frontend/src/decoder/onchain.ts` mirrors the Rust schemas via `@coral-xyz/borsh` primitives + `buffer-layout::blob` for fixed-size byte arrays. Public API exposes `bigint` and `Pubkey32 { hex }` shapes; raw `BN` values stay internal. A `SCHEMA_DESCRIPTOR` constant pins field order against `keeper_decoder::schema_descriptor_json()` — schema bumps that miss either side fail in CI on the next run.

**3. `WebSocketFeedAdapter` real subscription.** Replaces the wave-12 placeholder. Subscribes to `Connection.onAccountChange` (market PDA) + `onProgramAccountChange` (sub-pools, dormant buckets), routes by 8-byte discriminator (sha256(`"account:<TypeName>"`)[..8], computed at module load via `@noble/hashes`), decodes via the wave-14 TS Borsh layouts, holds back snapshots until the market PDA arrives, and dispatches a fully populated `FeedSnapshot` on each aggregator update. Injectable `connectionFactory` keeps the adapter unit-testable without a live websocket; 7 vitest tests cover subscription wiring, hold-until-market behavior, decode-failure tolerance, and clean teardown.

**4. `WindowWalletAdapter::signAndSubmit` real path.** Calls `window.solana.signAndSendTransaction(borshBytes)`; maps every wallet error class to `WalletSignError.kind`: `WalletNotConnected`, `NoTxBytes`, `ProviderMissing`, `ProviderUnsupported`, `UserRejected` (`code === 4001` or message contains "reject"/"denied"/"cancel"), `ProviderError`. Trader panels can now branch on `kind` for actionable banners. 11 vitest tests cover every error path + Backpack/Solflare/Phantom detection priority.

**5. `App.tsx` env wiring.** `buildAdapter()` reads `VITE_RPC_URL` + `VITE_MOLE_PROGRAM_ID` + `VITE_MARKET_PDA`; falls back to `MockFeedAdapter` if any is missing so default `npm run dev` keeps working without on-chain configuration.

Sandbox-blocked items still pushed forward to wave 15: `wasm-pack` build of `keeper-decoder.wasm` (the TS hand-rolled schemas in §2 are a stop-gap until then), `cargo build-sbf` + `solana-program-test` matrix (Solana toolchain still hits the sandbox bandwidth wall — 6th retry; CI runner closes the loop).

## Wave 15 (next)

1. **CI pulls the Solana toolchain** on a runner with stable bandwidth — `cargo build-sbf` produces `mole_option.so`, `solana-program-test` runs the wave-9 governance reject matrix, and CU measurements fill `Docs/Planning/21-Dormant存储与CU预算.md`.
2. **`wasm-pack build` of `keeper-decoder`** — replaces the wave-14 hand-rolled TS schemas with the Rust-compiled wasm artifact, eliminating the dual-implementation surface for good.
3. **On-chain `KeeperLeaderLock` PDA + `KeeperLeaderHeartbeat` ix** — replaces the wave-12 file-lock-style leader gauge with a real on-chain election, so multi-region keeper bots can hot-stand-by without coordination drift.
4. **Trader panel actually submits transactions** — closes the loop demo → devnet by feeding the wave-14 `WindowWalletAdapter::signAndSubmit` with real `borshBytes` from a wave-15 wasm tx-builder.
5. **Audit firm onboarding kicks off** — two firms via `SECURITY.md §5.5` + `CONTRIBUTING.md §3`. Bug bounty program goes live with $10k / $50k / $250k tiers per `SECURITY.md §4`.
