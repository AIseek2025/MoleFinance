# MoleOption — Security Policy & Invariant Catalogue

> **Status**: Wave 12 — initial publication. This document is the
> single source of truth for the protocol's security posture and
> the on-disk artifact a third-party auditor walks through before
> reading code.

This file enumerates:

1. **Threat model** — who attacks us, with what capabilities, for
   what goal.
2. **Cryptographic / economic invariants** — every property the
   protocol claims to preserve, with the test or assertion that
   pins it.
3. **Trust assumptions** — what we assume the world looks like and
   what happens when those assumptions break.
4. **Vulnerability disclosure** — how to report an issue and what
   reward you'll receive.
5. **Audit firm onboarding** — the package an external auditor
   needs the day they join.

Every claim here is cross-referenced to a numbered invariant in
the codebase or a planning doc. If you read this file end-to-end
and find an unsupported claim, that's a bug — please file via the
disclosure process below.

---

## 1 Threat model

### 1.1 In-scope adversaries

| ID | Adversary | Capabilities | Primary goal |
|----|-----------|--------------|--------------|
| **A-1** | Trader (single account) | Submit any well-formed Anchor instruction; read all on-chain state; choose their own positions. | Extract more than they deposited; pause others' withdrawals; force protocol insolvency. |
| **A-2** | Trader coalition (≤ N accounts) | Same as A-1 with `N` independent signers. | Coordinated rotation timing, sandwich keeper bot's `sync_pre_open` actions. |
| **A-3** | MEV searcher / Jito bundler | Atomic transaction bundling within a slot; back-running; priority-fee bidding. | Sandwich keeper init/sync; race the legitimate keeper to claim recovery. |
| **A-4** | Malicious keeper | Holds the `keeper_authority` seed and can sign keeper-only ix (signal `init_dormant_bucket`, `close_dormant_bucket`, etc). | Skim recovery; selectively delay keeper actions to harm a specific user; submit nonsensical hints to drain rent. |
| **A-5** | Compromised governance | Holds the `governance_authority`. | Bypass `pause_globally`/`pause_market` flags; ship malicious schema upgrade; siphon `pool_equity` directly. |
| **A-6** | Pyth oracle outage / manipulation | Suppress price updates; submit a single-block price spike (less than `oracle_max_slot_age`). | Force adverse mark-to-market; trigger spurious rotations. |
| **A-7** | Front-end / RPC infra adversary | Spoofs RPC responses; censors specific transactions; modifies the served frontend bundle. | Trick the user into signing the wrong instruction. |

### 1.2 Out-of-scope

- **Validator-level censorship beyond 2 epochs.** We rely on Solana's
  base liveness assumption.
- **51 % attack on Solana consensus.** If Solana is compromised, every
  Solana program is compromised.
- **Compromised user wallet / browser extension.** The user signs
  what they sign.
- **Physical / supply-chain attacks on the project team.** Mitigated
  by ops procedures (`Docs/Planning/24-operator-runbook.md` §3
  governance changes), not by code.

---

## 2 Invariant catalogue

Every invariant has a 4-letter prefix matching its enforcement layer:

- **CORE-** — pure-math invariant in `crates/clearing-core` /
  `crates/molemath`. Compiled-in; broken by a bug in the math.
- **ONCH-** — on-chain enforcement (`programs/mole-option`). Broken
  by a bug in the Anchor handler.
- **GOVN-** — governance / lockdown invariant (wave 9). Broken by a
  malicious `governance_authority` or a coding error in
  pause/freeze gating.
- **KEEP-** — keeper-bot operational invariant. Broken if the
  keeper is offline or buggy — protocol still solvent, but UX
  degrades.
- **OPS-** — operational invariant (depends on humans following the
  runbook).

### 2.1 Pool solvency (CORE)

| ID | Statement | Pinned by |
|----|-----------|-----------|
| **CORE-1** | `pool_equity ≥ Σ(open_position.collateral)` for every sub-pool, every direction, after every state mutation. | `crates/clearing-core/tests/properties.rs::prop_conservation_under_random_walk` (proptest, 10 000 seeds × 50–150 steps) |
| **CORE-2** | `Σ(loss_locked) ≤ Σ(deposited)` — losses are deduplicated and never exceed total inflows. | `crates/protocol-harness/tests/random_workload.rs::random_workload_preserves_all_invariants_across_seeds` |
| **CORE-3** | Mark-to-market computation uses checked fixed-point arithmetic; every multiplication path either succeeds, returns `MathError`, or panics in debug. No silent wrap-around. | `crates/molemath/src/tests.rs::mul_div_quotient_overflow` (+ `mul_div_handles_huge_intermediate`, `mul_div_div_by_zero`) |
| **CORE-4** | Rotation generation counters strictly monotonic per `(sub_pool, direction)`. Generation `N+1` is consumed only after `N` is fully synced. | `crates/clearing-core/tests/dormant_cycles.rs::lazy_migration_handles_two_consecutive_rotations` |

### 2.2 Bucket invariants (CORE + ONCH)

| ID | Statement | Pinned by |
|----|-----------|-----------|
| **CORE-5** | `DormantBucket.total_shares == Σ(position.shares)` for every active bucket; lazy-migration replay matches eager state byte-equal. | `crates/clearing-core/tests/dormant_lazy_equivalence.rs::lazy_eager_equivalence_with_aggressive_compaction` (random walks + invariant `inv5_recovery_shares_match_buckets`) |
| **CORE-6** | `DormantBucket.pending_recovery` decreases monotonically as recovery is paid; reaches exactly 0 before close. | `crates/protocol-harness/tests/rotation_focused.rs::multi_position_bucket_one_claims_then_others_recover_more` |
| **ONCH-1** | `init_dormant_bucket` rejects when the target `(sub_pool, direction, tick)` PDA already exists. | `crates/chain-mirror/src/tests.rs::keeper_preinit_rejects_duplicate` (mirrors Anchor `init` constraint host-side) |
| **ONCH-2** | `close_dormant_bucket` rejects when `pending_recovery > 0` OR `total_shares > 0`. | `crates/chain-mirror/src/tests.rs::keeper_close_dormant_bucket_after_full_drain` (positive-path lock; close path itself gated by engine `BucketNotDrained`) |
| **ONCH-3** | `sync_pre_open` is idempotent — calling it twice on the same generation has no effect. | `crates/clearing-core/tests/dormant_lazy_equivalence.rs::lazy_distribute_does_not_touch_buckets_until_apply` |

### 2.3 Per-block clearing (CORE + KEEP, wave 7-8)

| ID | Statement | Pinned by |
|----|-----------|-----------|
| **CORE-7** | `mark_to_market_distribution` updates only `SubPool` aggregate fields per block; no per-position iteration in the hot path. The lazy ledger materialises a position's gains only on its next touch. | `crates/clearing-core/tests/lazy_ledger.rs::lazy_replay_step_by_step_matches_full_batch` |
| **CORE-8** | `DistributionLedger` cumulative checkpoints satisfy `cum_at(t2) - cum_at(t1) ≡ Σ_{t∈(t1,t2]} block_distribution[t]` exactly under lazy + eager paths. | `crates/clearing-core/tests/lazy_ledger.rs::outstanding_claim_consistent_eager_vs_lazy` |
| **KEEP-1** | Keeper bot's `RotateRiskPredictor` picks the same imminent-rotation sub-pools given the same `(price, sub_pool_state)` — the σ̂ → Φ approximation is pure. | `crates/keeper/src/lib.rs::tests::predictor_surfaces_imminent_long_zero` (+ `predictor_skips_overcollateralised_long_pool`, `predictor_skips_empty_pool`) |

### 2.4 Wave-9 governance / lockdown invariants (GOVN)

| ID | Statement | Pinned by |
|----|-----------|-----------|
| **GOVN-1** | Setting `GlobalConfig.paused_globally=true` (or `Market.paused=true`) causes every funds-touching engine entrypoint to reject in a single tx with `ProtocolPaused`. | `crates/chain-mirror/src/tests.rs::governance_pause_immediately_rejects_every_funds_path` |
| **GOVN-2** | Pause is per-(market \| globally) — `Market.paused` rejects funds paths on that market while leaving siblings open; lockdown engine entrypoints in `clearing-core` go through one `assert_not_paused` call. | `crates/clearing-core/tests/safety_gates.rs::paused_blocks_sync_pool` (+ `paused_blocks_open_position_via_sync`, `paused_blocks_close_position`, `paused_blocks_force_close`, `paused_blocks_claim_dormant`, `paused_blocks_pre_sync_bucket`, `paused_blocks_harvest_dust`) |
| **GOVN-3** | Setting `Market.frozen_new_position=true` rejects only `open_position`; existing positions remain closeable. | `crates/clearing-core/tests/safety_gates.rs::frozen_new_position_blocks_open_only` (+ `crates/chain-mirror/src/tests.rs::governance_freeze_blocks_only_open_not_close` for end-to-end on-chain seam) |
| **GOVN-4** | `schema_version_current` mismatch between on-chain and signer rejects every instruction with `SchemaVersionMismatch` — the keystone defense against an admin-multisig premature schema bump. | `crates/clearing-core/tests/safety_gates.rs::sync_pool_rejects_stale_market_schema` (+ 6 sibling `*_rejects_stale_*_schema` tests across every funds path) and `crates/keeper-rpc/src/snapshot.rs::tests::refresh_rejects_schema_mismatch_when_enforced` (snapshot-side enforcement) |
| **GOVN-5** | `bump_market_schema_version` ahead of a deployed migration ix immediately freezes every funds path until ops runs `migrate_position`. The on-chain admin can't accidentally desync clients. | `crates/chain-mirror/src/tests.rs::governance_bump_without_program_upgrade_freezes_protocol` |
| **GOVN-6** | `migrate_position` walks the per-position `schema_version` strictly forward and is idempotent (re-running on an already-migrated position is a no-op). | `crates/chain-mirror/src/tests.rs::governance_migrate_position_walks_schema_forward_with_noop_guard` |

### 2.5 Wave-10 keeper bot integrity (KEEP)

| ID | Statement | Pinned by |
|----|-----------|-----------|
| **KEEP-2** | Keeper bot's `Scheduler::plan` emits a `CloseDormantBucket` action only when the bucket is fully drained (`is_dead == true` AND `pending_distribution == 0`); any live bucket goes to `PreSync` first. | `crates/keeper/src/lib.rs::tests::dead_bucket_with_pending_emits_pre_sync_first_not_close` (+ `dead_caught_up_bucket_emits_close`) |
| **KEEP-3** | Anchor instruction discriminators are derived from `sha256(b"global:<ix_name>")[..8]` and pinned at compile time. | `crates/tx-codec/src/lib.rs::tests::discriminator_constants_match_sha256_of_anchor_namespace` |
| **KEEP-4** | `record_init_hint` deduplicates against existing PDAs and is one-shot per emit — submitting the same hint twice plans only one `InitDormantBucket` action. | `crates/keeper/src/lib.rs::tests::init_hint_for_existing_pda_is_dropped` (+ `init_hint_is_one_shot_after_emit`) |
| **KEEP-5** | Keeper's `RealizedVolatilityEstimator` clamps σ̂ to `[min_clamp, max_clamp]` (default `[0.05, 5.0]`) — adversary-fed constant or spike prices can't poison the predictor outside the band. | `crates/keeper/src/lib.rs::tests::vol_estimator_constant_price_clamps_to_floor` (+ `vol_estimator_synthetic_walk_recovers_order_of_magnitude`, `vol_estimator_drops_out_of_order_and_duplicate_slots`) |

### 2.6 Wave-11 production RPC integrity (KEEP)

| ID | Statement | Pinned by |
|----|-----------|-----------|
| **KEEP-6** | `Pubkey32 → solana_pubkey::Pubkey → Pubkey32` is byte-equal — every keeper PDA computed off-chain points at the same on-chain account. | `crates/keeper-rpc/src/solana.rs::tests::pubkey_round_trip_is_byte_equal` |
| **KEEP-7** | `AccountMeta` (signer + writable flags) survive the conversion to `solana_instruction::AccountMeta` bit-for-bit. | `crates/keeper-rpc/src/solana.rs::tests::account_meta_conversion_preserves_flags` |
| **KEEP-8** | `DispatchedAction → solana_instruction::Instruction` preserves the data blob byte-equal. Combined with KEEP-3, this means Anchor sees exactly the discriminator we asserted at compile time. | `crates/keeper-rpc/src/solana.rs::tests::dispatched_to_instruction_preserves_bytes` |
| **KEEP-9** | Unreachable-RPC error path returns `RpcError::Transport`, never panics — keeper retries safely. | `crates/keeper-rpc/src/solana.rs::tests::fetch_account_against_unreachable_url_yields_transport_error` (+ 2 sibling tests) |

### 2.7 Wave-12 operational invariants (OPS)

| ID | Statement | Pinned by |
|----|-----------|-----------|
| **OPS-1** | The 18 health-check thresholds in `ops-toolkit` match the runbook §2 table line-for-line. Tightening any one requires updating both. | `crates/ops-toolkit/src/checks.rs::tests::*` (12 threshold tests) |
| **OPS-2** | Keeper bot's `is_transient` classification — *governance / structural errors are permanent, network errors are transient* — never mis-classifies. Mis-classification would either stall the keeper on a recoverable blip OR loop on an unrecoverable failure. | `crates/keeper-bot/src/run.rs::tests::governance_errors_are_permanent` + `rpc_transport_errors_are_transient` |
| **OPS-3** | Health-prober exit code respects the strict ordering `P0 critical > P1 critical > P2 critical > any warn > all pass`. AlertManager paging tier depends on this. | `crates/ops-toolkit/src/report.rs::tests::exit_code_picks_max_across_checks` |
| **OPS-4** | Prometheus exposition is always parseable: every emitted line is empty, starts with `#`, or matches `<name> <value>`. | `crates/keeper-bot/src/metrics.rs::tests::render_prometheus_emits_help_and_type_per_metric` |

---

## 3 Trust assumptions

We rely on the following — if any breaks, the protocol's safety
guarantees degrade as documented:

| ID | Assumption | If broken |
|----|------------|-----------|
| **T-1** | Solana base layer provides liveness within 2 slots p99. | Trades may stall; keeper queues build up. CORE-* invariants still hold. |
| **T-2** | Pyth oracle is honest within `oracle_confidence_ratio < 2 %`. | Outside that, Market trips into "stale" mode (oracle_slot_age > 64 → ops-toolkit P0 alert). Open/close return `OracleStale`. |
| **T-3** | `keeper_authority` keypair stays in the keeper bot's hot wallet (1 SOL gas budget, 1 of 1 signer). | A-4 takes over. Damage limited to delaying / front-running keeper actions; no fund extraction. Mitigation: rotate via `rotate_keeper` (GOVN-6). |
| **T-4** | `governance_authority` keypair is held in 3-of-5 multisig (Squads). | A-5 requires 3 signers; coordinated compromise of 3 multisig holders is possible but expensive. Mitigation: pause-then-rotate via GOVN-5 emergency. |
| **T-5** | Solana clock sysvar advances. (Slot stamps are non-decreasing.) | A clock regression would invalidate KEEP-5's slot-weighted vol estimator. Mitigation: vol estimator's out-of-order guard discards regressed samples. |

---

## 4 Vulnerability disclosure

### 4.1 Reporting channel

- **Primary**: `security@moleoption.example`. PGP key fingerprint:
  `TBD-WAVE-13` (rotated quarterly; latest fingerprint on the
  homepage's `/.well-known/security.txt`).
- **Backup**: encrypted Signal message to the on-call engineer
  (rotation in `Docs/Planning/24-operator-runbook.md` §5).
- **Anonymous**: GitHub Security Advisory on the `mole-option` repo.

We will acknowledge within **24 hours**, triage within
**72 hours**, and aim for a remediation patch within
**14 days for P0 / P1**, **30 days for P2 / P3**.

### 4.2 Bug bounty

Tier 0 (proof-of-concept exploit on devnet) — flat **$10K USDC**.
Tier 1 (any provable break of a CORE-* or ONCH-* invariant on
mainnet, no funds taken) — flat **$50K USDC**.
Tier 2 (any provable extraction of user funds on mainnet, ≥
$10K) — **the larger of $250K USDC or 10 % of funds preserved**.

Payouts are made within 30 days of the patch deploying to
mainnet. Repeat reporters get a permanent slot on the
"contributors" list.

### 4.3 Out-of-scope reports

- Test-net only attacks that do not generalise.
- Issues in third-party software (Phantom wallet UI, RPC providers).
  Please report those directly to the vendor.
- Theoretical issues without a concrete reproducer.
- Compute-budget exhaustion bugs that don't lead to lost funds —
  these are operational issues, file via the operator runbook.

---

## 5 Audit firm onboarding package

When an auditor joins the project, hand them this checklist. Every
item is something we already have on disk; none of it is "we'll
prepare it for you".

### 5.1 Reading order (~ 1 day)

1. `README.md` — workspace layout, what runs where.
2. `Docs/Planning/01-design-goals.md` — the protocol's mission
   statement.
3. `Docs/Planning/02-clearing-mechanism.md` — the heart of the
   on-chain design.
4. `Docs/Planning/06-loss-recovery.md` — the wave-6 loss
   recovery scheme. Most invariant-density.
5. `Docs/Planning/09-governance-and-lockdown.md` — wave-9
   pause / freeze / schema-upgrade design.
6. `Docs/Planning/20-攻坚开发进度与里程碑.md` — wave-by-wave delivery
   record. A timeline of what we built and why.
7. `Docs/Planning/23-on-chain-dormant-bridge.md` — bridge between
   off-chain keeper and on-chain dormant buckets.
8. `Docs/Planning/24-operator-runbook.md` — operations playbook.
9. `SECURITY.md` (this file) — invariant catalogue.

### 5.2 Code-side reading order (~ 2 days)

1. `crates/molemath/` — pure fixed-precision math. Read
   `src/lib.rs` end-to-end, ~300 LOC. Pinned by an exhaustive
   proptest suite.
2. `crates/clearing-core/` — protocol semantics in the abstract.
   Read `src/lib.rs` plus the `tests/` directory.
3. `programs/mole-option/src/` — on-chain Anchor handlers.
   Per-handler code review against the corresponding `Docs/Planning/`
   doc.
4. `crates/keeper/`, `crates/keeper-rpc/`, `crates/keeper-bot/` —
   off-chain keeper bot. Threat model A-4 lives here.
5. `crates/ops-toolkit/` — health prober.

### 5.3 Local environment reproduction

```bash
# 1. Repo + deps (everything pinned in Cargo.lock + workspace).
git clone https://github.com/MoleOption/MoleOption.git
cd MoleOption

# 2. Workspace-wide tests (wave 1-12, 200+ tests, ~30 s).
cargo test --workspace

# 3. Lints.
cargo clippy --workspace --all-targets -- -D warnings

# 4. Frontend smoke build.
cd frontend && npm install && npm run typecheck && npm run build

# 5. Production keeper bot daemon (mock fetcher).
cargo run -p keeper-bot -- serve 0.0.0.0:9099 5
# In another shell:
curl -s http://localhost:9099/metrics
curl -s http://localhost:9099/healthz

# 6. Health prober demo.
cargo run -p ops-toolkit -- demo human         # exit 0, all pass
cargo run -p ops-toolkit -- demo-broken json   # exit 4, P0 critical
```

### 5.4 What we want from an auditor

1. **Adversarial review of CORE-* / ONCH-* / GOVN-* invariants.** The
   pinned tests prove the math holds when the inputs are
   well-formed. We need an auditor to *find inputs that break it*.
2. **Review of every PDA seed string.** The wrong seed leads to
   the wrong account; this is the single highest-impact class of
   Anchor bug. Cross-reference our seeds against `Docs/Planning/05-account-layout.md`.
3. **Compute budget review.** Production CU costs are estimated
   in `Docs/Planning/21-cu-budget.md` (wave-12 still pending real
   measurement; see §10 of `Docs/Planning/20-…md`). We need
   "cargo build-sbf" runs and exact CU counts per instruction at
   p50 / p99 input sizes.
4. **Review of wallet & RPC paths in `crates/keeper-rpc/src/solana.rs`.**
   In particular: `decode_account_data_blob` and
   `dispatched_to_instruction`. KEEP-6..9 pin byte equality — make
   sure those tests are sufficient.
5. **Frontend wallet adapter review** (when wave-13 lands real
   wallet wiring). Wave-12 only ships the seam; wave-13 gets
   serious about transaction encoding.

### 5.5 Out-of-scope for the audit

- The mock fetcher / mock data feed used in tests.
- The wave-12 frontend mock adapter (no funds at risk).
- The `chain-mirror` test harness (test-only).
- The `simulation` and `protocol-harness` crates (test-only).

---

## 6 Update policy

This document is **versioned with the source code**. Any change to
an invariant — adding, removing, tightening — MUST be accompanied
by an update here in the same commit. CI verifies that every
`### 2.x` test reference in this file points to a real test by
running `grep` against the workspace; if the reference goes stale,
the build fails (wave-12 task: scaffolded; wave-13 wires the CI
hook).

We tag a fresh PGP-signed snapshot of `SECURITY.md` at every
mainnet release.

---

*Last updated: wave 12. Next scheduled review: wave 13 (when CI
hook lands).*
