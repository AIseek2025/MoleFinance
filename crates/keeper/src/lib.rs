//! Host-side keeper scheduler.
//!
//! Wave 8 deliverable. The on-chain protocol relies on an off-chain
//! keeper to do three things between user transactions:
//!
//! 1. **`pre_sync_dormant_bucket`** — drain pending lazy-mode
//!    distribute entries into a bucket so the ledger ring buffer
//!    doesn't fill up and so user `claim_dormant_recovery` calls see
//!    fully-applied accruals. Trigger: `bucket.last_applied_index <
//!    ledger.next_event_index`.
//! 2. **`close_dormant_bucket`** — reclaim rent on PDAs the engine
//!    has fully drained. Trigger: `bucket.is_dead() &&
//!    bucket.last_applied_index >= ledger.next_event_index`.
//! 3. **`init_dormant_bucket`** *(v1: opt-in)* — pre-allocate a PDA
//!    for an upcoming rotate. Decided by an external risk model;
//!    surfaced through [`Scheduler::record_init_hint`].
//!
//! ## Design
//!
//! This crate is intentionally **pure-host Rust**. It exposes the
//! [`KeeperChainView`] trait describing the read-only slice of chain
//! state the scheduler needs (buckets, ledgers, sub-pools). Production
//! deployments wire a Solana RPC client to that trait; tests use
//! `chain-mirror`. The scheduler returns a [`Vec<KeeperAction>`]
//! priority-sorted so the caller can submit the most urgent
//! transactions first.
//!
//! ## What this crate is NOT
//!
//! - It does not submit Solana transactions. That belongs in a
//!   separate `keeper-bot` binary that consumes this crate's output.
//! - It does not predict where rotations will occur from market
//!   prices. That belongs in a risk-modelling layer that calls
//!   [`Scheduler::record_init_hint`].
//! - It does not enforce SLAs or latency budgets — those are deploy-
//!   time concerns above this layer.
//!
//! See `Docs/Planning/23-on-chain-dormant-bridge.md` § wave-8 for the
//! full keeper architecture and rollout plan.

#![deny(missing_docs)]

use std::collections::HashMap;

use clearing_core::Direction;

// =====================================================================
// Errors
// =====================================================================

/// Errors returned by the keeper scheduler.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeeperError {
    /// A chain view returned a bucket whose `last_applied_index` is
    /// strictly greater than the parent ledger's `next_event_index`.
    /// This is an off-chain reconstruction error; the on-chain
    /// invariant `last_applied_index <= next_event_index` is enforced
    /// by `clearing_core::DormantStore::apply_pending_to_bucket`.
    #[error("invariant violated: bucket last_applied_index ({applied}) > ledger next_event_index ({head}) for ({sub_pool}, {direction:?}, tick={tick})")]
    BucketAheadOfLedger {
        /// Sub pool id.
        sub_pool: u32,
        /// Direction.
        direction: Direction,
        /// Bucket tick.
        tick: i64,
        /// `bucket.last_applied_index`.
        applied: u64,
        /// `ledger.next_event_index`.
        head: u64,
    },
    /// The chain view returned a bucket but no parent ledger for its
    /// `(sub_pool, direction)`. Always a programming error.
    #[error("chain view: missing ledger for ({sub_pool}, {direction:?})")]
    MissingLedger {
        /// Sub pool id.
        sub_pool: u32,
        /// Direction.
        direction: Direction,
    },
}

// =====================================================================
// Chain view trait
// =====================================================================

/// Read-only snapshot of one dormant bucket — exactly the engine-
/// observable shape the bridge persists on chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketSnapshot {
    /// Sub pool id this bucket belongs to.
    pub sub_pool_id: u32,
    /// Direction.
    pub direction: Direction,
    /// Bucket tick.
    pub tick: i64,
    /// Anchor price stamped on the bucket.
    pub anchor_price: u64,
    /// Aggregated recovery shares.
    pub total_recovery_shares: u128,
    /// Aggregated dormant notional.
    pub total_recovery_notional: u128,
    /// Funds attributed to this bucket; redeemable by claim.
    pub accrued_value: u128,
    /// Number of distinct dormant positions held in this bucket.
    pub position_count: u64,
    /// Index of the last `DistEntry` applied to this bucket.
    pub last_applied_index: u64,
}

impl BucketSnapshot {
    /// Mirror of [`clearing_core::OnChainBucketRecord::is_dead`] —
    /// kept duplicated here so callers don't need to construct a full
    /// `OnChainBucketRecord` to ask the question.
    #[inline]
    pub fn is_dead(&self) -> bool {
        self.total_recovery_shares == 0
            && self.total_recovery_notional == 0
            && self.accrued_value == 0
            && self.position_count == 0
    }
}

/// Minimal view of one [`crate::clearing_core::DistributionLedger`]
/// PDA's mutable fields the keeper cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerSnapshot {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Direction.
    pub direction: Direction,
    /// Absolute index just past the last appended entry.
    pub next_event_index: u64,
    /// Number of buckets the engine *expects* to exist (sub-pool's
    /// `<dir>_dormant_bucket_count`). The keeper compares this to
    /// the chain-view `buckets()` length to surface "missing PDA"
    /// drift.
    pub bucket_count_hint: u32,
}

/// **Wave 9.** Read-only health snapshot of one sub-pool's *active*
/// state. Used by [`RotateRiskPredictor`] to estimate how close each
/// directional pool is to zero-equity (the rotate trigger), without
/// loading the full `clearing_core::SubPool` shape.
///
/// All fields mirror their on-chain `SubPool` account counterpart
/// 1:1; the predictor never derives them — it consumes them as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubPoolHealth {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Last oracle price written by `sync_pool` (PRICE_SCALE units).
    pub last_price: u64,
    /// Anchor price snapshot for the long active generation. Used
    /// to derive `tick_for_zero_price` once the engine projects the
    /// pool to zero. If long is in pristine pre-rotate state, this
    /// equals `last_price`.
    pub long_anchor_price: u64,
    /// Anchor price snapshot for the short active generation.
    pub short_anchor_price: u64,
    /// Long-side active pool equity (denominated in collateral
    /// minor units). Reaches 0 → triggers a long rotation.
    pub long_pool_equity: u128,
    /// Short-side active pool equity.
    pub short_pool_equity: u128,
    /// Long-side active notional (sum of position notionals
    /// outstanding in the long active generation).
    pub long_active_notional: u128,
    /// Short-side active notional.
    pub short_active_notional: u128,
    /// Active generation tag for long. Surfaced to the predictor
    /// so it can carry it into the recommended `init_dormant_bucket`
    /// metadata.
    pub long_active_generation: u64,
    /// Active generation tag for short.
    pub short_active_generation: u64,
}

/// What the keeper needs to read from chain to make decisions. Any
/// implementor (live RPC, indexer cache, chain-mirror) can plug in.
pub trait KeeperChainView {
    /// All sub-pool ids visible on chain.
    fn sub_pool_ids(&self) -> Vec<u32>;
    /// Every bucket PDA snapshot for the given sub pool, in
    /// deterministic order (caller-defined).
    fn buckets(&self, sub_pool_id: u32) -> Vec<BucketSnapshot>;
    /// The two ledger PDAs (long, short) for the given sub pool.
    /// `None` if the sub pool does not exist.
    fn ledger(&self, sub_pool_id: u32, direction: Direction) -> Option<LedgerSnapshot>;
    /// **Wave 9.** Optional sub-pool health snapshot. Implementors
    /// that haven't wired the new fields yet can keep the default
    /// (`None`); [`RotateRiskPredictor`] silently skips sub-pools
    /// without a health view.
    fn sub_pool_health(&self, _sub_pool_id: u32) -> Option<SubPoolHealth> {
        None
    }
}

// =====================================================================
// Actions
// =====================================================================

/// Why an [`KeeperAction::InitDormantBucket`] was scheduled. Reserved
/// for v1.5 risk-model integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitRationale {
    /// External risk model predicted a rotate to this tick within the
    /// configured horizon.
    RotateRiskHorizon,
    /// Caller explicitly requested the init via
    /// [`Scheduler::record_init_hint`] without giving a reason.
    Explicit,
}

/// Concrete keeper task with metadata; consumed by the keeper bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeeperAction {
    /// Drain pending lazy-mode distribute entries into a single
    /// bucket. Consumes one `pre_sync_dormant_bucket(direction,
    /// bucket_tick)` instruction on chain.
    PreSyncDormantBucket {
        /// Sub pool id.
        sub_pool_id: u32,
        /// Direction.
        direction: Direction,
        /// Bucket tick.
        tick: i64,
        /// Number of pending entries waiting for this bucket
        /// (`ledger.next_event_index - bucket.last_applied_index`).
        /// Used as the priority signal: more pending = more urgent.
        pending: u64,
    },
    /// Reclaim rent from a dead, fully-caught-up bucket. Consumes one
    /// `close_dormant_bucket` instruction on chain.
    CloseDormantBucket {
        /// Sub pool id.
        sub_pool_id: u32,
        /// Direction.
        direction: Direction,
        /// Bucket tick.
        tick: i64,
    },
    /// Pre-allocate a fresh dead PDA so a forthcoming rotate can land.
    /// Consumes one `init_dormant_bucket(tick)` instruction on chain.
    /// v1: only emitted when a caller has supplied an explicit hint
    /// via [`Scheduler::record_init_hint`].
    InitDormantBucket {
        /// Sub pool id.
        sub_pool_id: u32,
        /// Direction.
        direction: Direction,
        /// Bucket tick.
        tick: i64,
        /// Rationale.
        rationale: InitRationale,
    },
}

impl KeeperAction {
    /// Action priority — higher number = more urgent. Used to order
    /// the queue [`Scheduler::plan`] returns. Tuned so init hints
    /// always run *before* pre-syncs (a missing PDA blocks user
    /// txns), pre-syncs run before closes, and within a category
    /// urgency scales with pending depth.
    pub fn priority(&self) -> u64 {
        match self {
            // Init has the highest absolute floor: a missing PDA can
            // surface as a user-facing tx revert via `BucketSlotExhausted`.
            KeeperAction::InitDormantBucket { .. } => 1_000_000_000,
            // Pre-sync priority scales with pending depth so the
            // most-behind bucket runs first.
            KeeperAction::PreSyncDormantBucket { pending, .. } => {
                10_000_000u64.saturating_add(*pending)
            }
            // Close is best-effort rent recovery; runs after the
            // critical paths.
            KeeperAction::CloseDormantBucket { .. } => 1,
        }
    }
}

// =====================================================================
// Scheduler
// =====================================================================

/// Tunable knobs for [`Scheduler::plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerConfig {
    /// Skip pre-sync actions whose `pending` count is below this
    /// threshold — avoids spamming low-value txns when the keeper is
    /// already keeping up. Default `1` so any non-zero pending fires.
    pub min_pending_for_pre_sync: u64,
    /// Cap the total number of actions the scheduler emits per plan
    /// call. Useful for keepers running on a tx budget. `0` = no cap.
    pub max_actions_per_plan: usize,
    /// When true, the scheduler emits `CloseDormantBucket` for every
    /// dead, caught-up bucket. Default `true`. Set `false` during
    /// initial rollout so a buggy `record_is_dead` predicate cannot
    /// accidentally close a still-live PDA.
    pub close_dead_buckets: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            min_pending_for_pre_sync: 1,
            max_actions_per_plan: 0,
            close_dead_buckets: true,
        }
    }
}

/// Stateful scheduler that turns a chain view into an action queue.
/// The scheduler holds two pieces of mutable state across plan calls:
///
/// 1. **Init hints** — explicit (sub_pool, direction, tick) ticks
///    queued by callers via [`Scheduler::record_init_hint`]. Cleared
///    when the corresponding PDA appears in the chain view OR when
///    the hint has been emitted as an action via [`Scheduler::plan`].
///    The scheduler does not auto-detect rotate risk in v1.
/// 2. **`SchedulerConfig`** — see field docs.
#[derive(Debug, Clone, Default)]
pub struct Scheduler {
    config: SchedulerConfig,
    init_hints: HashMap<(u32, Direction, i64), InitRationale>,
}

impl Scheduler {
    /// Construct with the given configuration.
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            config,
            init_hints: HashMap::new(),
        }
    }

    /// Replace the active configuration.
    pub fn set_config(&mut self, config: SchedulerConfig) {
        self.config = config;
    }

    /// Currently active configuration (read-only borrow).
    pub fn config(&self) -> &SchedulerConfig {
        &self.config
    }

    /// Register a hint that the keeper should pre-init the given PDA.
    /// Idempotent — duplicate hints are deduplicated. Hints are
    /// drained on the next [`Scheduler::plan`] call, after which the
    /// caller is responsible for re-recording if the PDA has not yet
    /// materialised.
    pub fn record_init_hint(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        tick: i64,
        rationale: InitRationale,
    ) {
        self.init_hints
            .insert((sub_pool_id, direction, tick), rationale);
    }

    /// Remove an init hint without surfacing it. Useful when the
    /// caller decides the rotate risk no longer applies.
    pub fn cancel_init_hint(&mut self, sub_pool_id: u32, direction: Direction, tick: i64) {
        self.init_hints.remove(&(sub_pool_id, direction, tick));
    }

    /// Number of pending init hints — primarily for tests / metrics.
    pub fn pending_init_hint_count(&self) -> usize {
        self.init_hints.len()
    }

    /// Compute the keeper action queue from the given chain view.
    /// Returned actions are sorted by descending [`KeeperAction::priority`].
    ///
    /// The scheduler is **stateless across plan calls** for pre-sync
    /// and close decisions: every plan call re-derives them from the
    /// current chain view. Init hints, in contrast, persist until the
    /// chain view confirms the corresponding PDA exists OR the
    /// scheduler emits the matching action; the caller is then
    /// expected to verify on chain and re-record on failure.
    pub fn plan(
        &mut self,
        view: &dyn KeeperChainView,
    ) -> Result<Vec<KeeperAction>, KeeperError> {
        let mut actions: Vec<KeeperAction> = Vec::new();

        for sub_pool_id in view.sub_pool_ids() {
            // Cache ledger snapshots per direction so we don't ask
            // the view twice per bucket.
            let mut ledgers: HashMap<Direction, LedgerSnapshot> = HashMap::with_capacity(2);
            for direction in [Direction::Long, Direction::Short] {
                if let Some(l) = view.ledger(sub_pool_id, direction) {
                    ledgers.insert(direction, l);
                }
            }

            let buckets = view.buckets(sub_pool_id);
            let mut existing_init_keys: HashMap<(u32, Direction, i64), bool> =
                HashMap::with_capacity(buckets.len());

            for b in &buckets {
                existing_init_keys.insert((b.sub_pool_id, b.direction, b.tick), true);

                // Validate ledger presence first.
                let ledger = ledgers.get(&b.direction).ok_or(KeeperError::MissingLedger {
                    sub_pool: b.sub_pool_id,
                    direction: b.direction,
                })?;

                // Sanity invariant: `last_applied_index <=
                // next_event_index`. The chain enforces it; an
                // off-chain reconstruction that violates it must be
                // surfaced loudly.
                if b.last_applied_index > ledger.next_event_index {
                    return Err(KeeperError::BucketAheadOfLedger {
                        sub_pool: b.sub_pool_id,
                        direction: b.direction,
                        tick: b.tick,
                        applied: b.last_applied_index,
                        head: ledger.next_event_index,
                    });
                }

                let pending = ledger.next_event_index - b.last_applied_index;

                if pending >= self.config.min_pending_for_pre_sync && pending > 0 {
                    actions.push(KeeperAction::PreSyncDormantBucket {
                        sub_pool_id: b.sub_pool_id,
                        direction: b.direction,
                        tick: b.tick,
                        pending,
                    });
                    // Don't also emit Close — pre_sync_dormant_bucket
                    // is a write that may revive shares (no, it
                    // can't; apply only adds value to dead buckets if
                    // they're activated, but a dead bucket has no
                    // shares so apply is a no-op — still, we want
                    // pre-sync first to preserve close-after-drain
                    // ordering).
                    continue;
                }

                if self.config.close_dead_buckets
                    && b.is_dead()
                    && b.last_applied_index >= ledger.next_event_index
                {
                    actions.push(KeeperAction::CloseDormantBucket {
                        sub_pool_id: b.sub_pool_id,
                        direction: b.direction,
                        tick: b.tick,
                    });
                }
            }
        }

        // Drain init hints. Skip hints whose PDA already exists.
        let to_emit: Vec<((u32, Direction, i64), InitRationale)> = self
            .init_hints
            .iter()
            .filter(|((sp, dir, tick), _)| {
                view.buckets(*sp)
                    .iter()
                    .all(|b| !(b.direction == *dir && b.tick == *tick))
            })
            .map(|(k, v)| (*k, *v))
            .collect();
        for ((sub_pool_id, direction, tick), rationale) in &to_emit {
            actions.push(KeeperAction::InitDormantBucket {
                sub_pool_id: *sub_pool_id,
                direction: *direction,
                tick: *tick,
                rationale: *rationale,
            });
        }
        // Init hints are one-shot: clear them after emitting.
        // Caller must re-record if the on-chain init failed.
        for (key, _) in &to_emit {
            self.init_hints.remove(key);
        }

        // Sort by descending priority. Stable so ties keep their
        // chain-view discovery order.
        actions.sort_by_key(|b| std::cmp::Reverse(b.priority()));

        if self.config.max_actions_per_plan > 0
            && actions.len() > self.config.max_actions_per_plan
        {
            actions.truncate(self.config.max_actions_per_plan);
        }

        Ok(actions)
    }
}

// =====================================================================
// Wave 9 — Rotate-Risk Predictor
// =====================================================================

/// Fixed reference: 365 * 24 * 3600. Matches the canonical annualised-
/// volatility convention used by financial libs (`σ_annual *
/// sqrt(T_years)`). Solana validator schedules one slot ≈ 400 ms,
/// but slot rate fluctuates; callers feed an average via
/// [`PredictorConfig::slots_per_second`].
const SECONDS_PER_YEAR: f64 = 365.0 * 24.0 * 3600.0;

/// Tunable inputs for [`RotateRiskPredictor`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PredictorConfig {
    /// Annualised log-return volatility of the underlying. E.g.
    /// `0.6` for a 60 % vol asset. Calibrated by the keeper bot
    /// from a realised-vol estimator.
    pub annual_vol: f64,
    /// How far into the future the predictor looks (slots).
    /// Roughly `5 * 60 / slot_seconds` for "5 minutes ahead". Larger
    /// horizons surface more candidate ticks but also more noise.
    pub horizon_slots: u64,
    /// Average slot duration in seconds. Solana mainnet ≈ 0.4.
    /// Used to convert `horizon_slots` into a horizon in years for
    /// the volatility model.
    pub slots_per_second: f64,
    /// Minimum probability for the predictor to surface a tick.
    /// Default `0.05` (≈5%); below this, the rotate is too unlikely
    /// to justify the init's rent + CU cost.
    pub min_probability: f64,
    /// `tick_aggregation_factor` — must match the
    /// `MarketParams::tick_aggregation_factor` of the target market.
    /// The predictor floors the projected zero-price to a tick
    /// boundary before emitting; mismatched values silently emit
    /// hints the engine never lands on.
    pub tick_aggregation_factor: u32,
    /// Price tick size — same source as
    /// `MarketParams::price_tick`.
    pub price_tick: u64,
}

impl Default for PredictorConfig {
    fn default() -> Self {
        Self {
            annual_vol: 0.6,
            horizon_slots: 1_500,        // ≈10 minutes at 0.4s/slot
            slots_per_second: 2.5,
            min_probability: 0.05,
            tick_aggregation_factor: 1,
            price_tick: 1,
        }
    }
}

/// **Wave 10.** Single oracle observation feeding
/// [`RealizedVolatilityEstimator`].
///
/// Deliberately NOT mirroring `clearing_core::PriceEnvelope` — the
/// vol estimator only needs the *trusted* `(price, slot)` pair the
/// pyth-adapter already validated. Keeping the type local also lets
/// the estimator stay in `keeper` without pulling `pyth_adapter` in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceSample {
    /// Price in the engine's `PRICE_SCALE` units (`1e8`).
    pub price: u64,
    /// Solana slot the price was observed at.
    pub slot: u64,
}

/// **Wave 10.** Rolling-window realised-volatility estimator. Wave-9
/// shipped a hand-tuned `PredictorConfig::annual_vol`; wave-10 closes
/// the auto-tuning loop by reading the same oracle history the engine
/// reads and producing a windowed σ̂ in the format
/// [`RotateRiskPredictor`] consumes verbatim.
///
/// ## Estimator
///
/// Annualised vol from a rolling window of N most recent log-returns:
///
/// ```text
/// r_i  = ln(p_i / p_{i-1})
/// Δt_i = (slot_i − slot_{i-1}) / slots_per_second / SECONDS_PER_YEAR
/// σ̂²  = (1 / Σ Δt_i) · Σ r_i²
/// σ̂   = √σ̂²
/// ```
///
/// Note: this is the canonical "scaled-by-time" realised-vol formula
/// (`Σr² / Σ Δt`), which preserves correctness when slot rate
/// fluctuates. The simpler `√(N/T)·stddev(r)` formulation assumes
/// equally-spaced samples — Solana slots can vary by ±20%, so we use
/// the time-weighted form.
///
/// ## Operational contract
///
/// - Out-of-order slots (a sample with `slot <= last_slot`) are
///   silently dropped. The keeper bot polls the oracle at a fixed
///   cadence; we never want the estimator to swallow stale RPC
///   responses or to surface negative time intervals.
/// - Below [`RealizedVolatilityEstimatorConfig::min_samples`]
///   observations, `current_estimate()` returns `None` (the keeper
///   should use `PredictorConfig::default().annual_vol` until the
///   estimator warms up).
/// - The window is bounded by both `max_samples` AND
///   `max_age_slots`; the more restrictive of the two evicts. This
///   matters during prolonged market quiet (max_samples preserves
///   recent history) and during slot-rate spikes (max_age_slots
///   prevents stale samples from drowning new data).
#[derive(Debug, Clone)]
pub struct RealizedVolatilityEstimator {
    config: RealizedVolatilityEstimatorConfig,
    samples: std::collections::VecDeque<PriceSample>,
}

/// Tunable knobs for [`RealizedVolatilityEstimator`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RealizedVolatilityEstimatorConfig {
    /// Hard cap on the number of samples retained. Bounded so the
    /// estimator's memory footprint is `O(max_samples)` regardless
    /// of how long the keeper bot has been running.
    pub max_samples: usize,
    /// Maximum age (in slots) any sample may have. Older samples
    /// are evicted on `record`. Use this to taper the window during
    /// slot-rate spikes.
    pub max_age_slots: u64,
    /// Minimum number of distinct samples required before the
    /// estimator reports a non-`None` value.
    pub min_samples: usize,
    /// Slots per second. Same role as [`PredictorConfig::slots_per_second`].
    /// Required so that "time-weighted" sums of `Δslot` translate to
    /// real seconds → years.
    pub slots_per_second: f64,
    /// Floor and cap for the produced `σ̂`. The keeper bot may want
    /// to clamp to a sane band so a transient outlier window can't
    /// inject `σ ≈ 0` (predictor under-counts) or `σ → ∞` (predictor
    /// over-emits init hints). Default `[0.05, 5.0]` matches typical
    /// crypto-asset annualised-vol observations.
    pub min_clamp: f64,
    /// Upper clamp; see `min_clamp`.
    pub max_clamp: f64,
}

impl Default for RealizedVolatilityEstimatorConfig {
    fn default() -> Self {
        Self {
            max_samples: 256,
            max_age_slots: 6 * 60 * 60 * 5 / 2,   // 6 hours @ 0.4s/slot
            min_samples: 16,
            slots_per_second: 2.5,
            min_clamp: 0.05,
            max_clamp: 5.0,
        }
    }
}

impl Default for RealizedVolatilityEstimator {
    fn default() -> Self {
        Self::new(RealizedVolatilityEstimatorConfig::default())
    }
}

impl RealizedVolatilityEstimator {
    /// Construct with the given configuration.
    pub fn new(config: RealizedVolatilityEstimatorConfig) -> Self {
        Self {
            config,
            samples: std::collections::VecDeque::with_capacity(config.max_samples),
        }
    }

    /// Read-only borrow of the active config.
    pub fn config(&self) -> &RealizedVolatilityEstimatorConfig {
        &self.config
    }

    /// Number of samples currently retained.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Drop every retained sample. Called by the keeper bot at
    /// market-pause transitions so that the post-resume window
    /// doesn't blend pre-pause and post-resume regimes.
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Record one sample. Out-of-order or duplicate slots are
    /// dropped. After insertion, evicts samples older than
    /// `max_age_slots` and trims the queue to `max_samples`.
    pub fn record(&mut self, sample: PriceSample) {
        if let Some(last) = self.samples.back() {
            if sample.slot <= last.slot {
                // Out-of-order or duplicate: drop silently.
                return;
            }
        }
        // Reject zero-price samples — log(0) is undefined; a real
        // pyth-adapter validated price is always > 0.
        if sample.price == 0 {
            return;
        }
        self.samples.push_back(sample);
        // Evict by age first.
        let cutoff = sample.slot.saturating_sub(self.config.max_age_slots);
        while let Some(front) = self.samples.front() {
            if front.slot < cutoff {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        // Then by count.
        while self.samples.len() > self.config.max_samples {
            self.samples.pop_front();
        }
    }

    /// Compute the rolling realised-vol estimate. Returns `None` if
    /// fewer than `min_samples` distinct observations are retained,
    /// or if the cumulative time-span across the window is zero
    /// (cannot scale variance to a per-year basis without it).
    pub fn current_estimate(&self) -> Option<f64> {
        if self.samples.len() < self.config.min_samples {
            return None;
        }
        let mut sum_r2 = 0.0;
        let mut sum_dt_years = 0.0;
        let mut prev: Option<&PriceSample> = None;
        for s in &self.samples {
            if let Some(p) = prev {
                let r = (s.price as f64).ln() - (p.price as f64).ln();
                let dslot = s.slot.saturating_sub(p.slot) as f64;
                let dt_years = dslot / self.config.slots_per_second / SECONDS_PER_YEAR;
                if dt_years > 0.0 && r.is_finite() {
                    sum_r2 += r * r;
                    sum_dt_years += dt_years;
                }
            }
            prev = Some(s);
        }
        if sum_dt_years <= 0.0 {
            return None;
        }
        let sigma2_annual = sum_r2 / sum_dt_years;
        let sigma = sigma2_annual.sqrt();
        if !sigma.is_finite() {
            return None;
        }
        Some(sigma.clamp(self.config.min_clamp, self.config.max_clamp))
    }

    /// Apply the current estimate to a [`PredictorConfig`] in
    /// place. No-op when the estimator hasn't warmed up yet so the
    /// caller's hand-tuned default survives the boot window.
    pub fn apply_to_predictor_config(&self, predictor: &mut PredictorConfig) -> bool {
        if let Some(sigma) = self.current_estimate() {
            predictor.annual_vol = sigma;
            true
        } else {
            false
        }
    }
}

/// Result of a single rotate-risk evaluation. One per (sub_pool_id,
/// direction) pair *if* the engine projects a zero-equity event with
/// probability above [`PredictorConfig::min_probability`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotatePrediction {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Direction whose active pool is at risk of hitting zero.
    pub direction: Direction,
    /// Estimated price at which `<direction>_pool_equity` reaches 0.
    pub zero_price: u64,
    /// Bucket tick the engine will land the rotate on (the floor of
    /// `zero_price` to the configured aggregation grid).
    pub zero_tick: i64,
    /// Probability the underlying touches `zero_price` within the
    /// horizon, evaluated under the geometric-Brownian-motion
    /// assumption. Range `[0, 1]`.
    pub probability: f64,
}

/// **Wave 9.** Fills [`Scheduler::record_init_hint`] from a price-
/// volatility model. The math is intentionally simple — a one-touch
/// approximation under geometric Brownian motion — because the
/// predictor's job is not to be precise but to surface "this PDA
/// might be needed soon" with sub-second latency. Mistakes are
/// cheap (an extra rent deposit, refundable via
/// `close_dormant_bucket`); missing a real rotate is expensive (the
/// next user tx reverts with `BucketSlotExhausted` until the keeper
/// catches up).
///
/// ## Model
///
/// Let `S₀ = last_price`, `σ = annual_vol`, `T = horizon_slots /
/// slots_per_second / SECONDS_PER_YEAR`. The log-return at horizon
/// is `X ~ N(0, σ²·T)`. We approximate the *one-touch* probability
/// of reaching `S_zero` (a barrier below `S₀` for long, above for
/// short) by the Gaussian terminal probability — a known undercount,
/// but tight enough for "is the PDA needed?" decision making at the
/// `min_probability >= 5%` regime.
///
/// ## Zero-price projection
///
/// The directional pool reaches zero equity when realized P&L
/// exhausts deposits:
///
/// - **Long**: equity ≈ `long_pool_equity + long_active_notional *
///   (S/S_anchor − 1)`. Setting this to 0:
///   `S_zero_long = long_anchor_price · (1 − long_pool_equity / long_active_notional)`.
/// - **Short**: equity ≈ `short_pool_equity + short_active_notional
///   * (1 − S/S_anchor)`. Setting this to 0:
///   `S_zero_short = short_anchor_price · (1 + short_pool_equity / short_active_notional)`.
///
/// Notes:
/// - If `<dir>_active_notional == 0`, the pool has no positions, so
///   no rotate can fire — return `None`.
/// - If the projected zero-price has already been crossed (e.g.
///   long zero-price > current price for a long pool, which means
///   equity is already negative — the engine would have force-
///   rotated by now), the predictor still emits the recommendation
///   so the keeper races the next sync.
#[derive(Debug, Clone, Copy)]
pub struct RotateRiskPredictor {
    config: PredictorConfig,
}

impl RotateRiskPredictor {
    /// Construct with the given config.
    pub fn new(config: PredictorConfig) -> Self {
        Self { config }
    }

    /// Read-only borrow of the active config.
    pub fn config(&self) -> &PredictorConfig {
        &self.config
    }

    /// Replace the active config.
    pub fn set_config(&mut self, config: PredictorConfig) {
        self.config = config;
    }

    /// Run the model on a single sub-pool and return up to two
    /// predictions (one per direction). Predictions below
    /// `min_probability` are filtered.
    pub fn predict_one(&self, h: &SubPoolHealth) -> Vec<RotatePrediction> {
        let mut out = Vec::with_capacity(2);
        if let Some(p) = self.predict_direction(h, Direction::Long) {
            out.push(p);
        }
        if let Some(p) = self.predict_direction(h, Direction::Short) {
            out.push(p);
        }
        out
    }

    /// Run the model on every sub-pool the view exposes, push init
    /// hints into the given scheduler for every prediction whose
    /// probability passes [`PredictorConfig::min_probability`], and
    /// return the predictions for inspection. Caller can then call
    /// [`Scheduler::plan`] to materialise the resulting actions.
    pub fn populate_scheduler(
        &self,
        view: &dyn KeeperChainView,
        scheduler: &mut Scheduler,
    ) -> Vec<RotatePrediction> {
        let mut all = Vec::new();
        for sub_pool_id in view.sub_pool_ids() {
            let Some(h) = view.sub_pool_health(sub_pool_id) else {
                continue;
            };
            for pred in self.predict_one(&h) {
                scheduler.record_init_hint(
                    pred.sub_pool_id,
                    pred.direction,
                    pred.zero_tick,
                    InitRationale::RotateRiskHorizon,
                );
                all.push(pred);
            }
        }
        all
    }

    fn predict_direction(
        &self,
        h: &SubPoolHealth,
        dir: Direction,
    ) -> Option<RotatePrediction> {
        if h.last_price == 0 {
            return None;
        }
        let (anchor, equity, notional) = match dir {
            Direction::Long => (h.long_anchor_price, h.long_pool_equity, h.long_active_notional),
            Direction::Short => (h.short_anchor_price, h.short_pool_equity, h.short_active_notional),
        };
        if anchor == 0 || notional == 0 {
            return None;
        }
        // Equity / notional in [0, ~1+]. We clamp to [0, 1] for the
        // direction the model is sensible for — a > 1 ratio means the
        // pool is *over*-collateralised, which is fine for short
        // (zero would be at S = 2·anchor) but for long would drop
        // S_zero below 0 (impossible). Both cases are handled by
        // saturating arithmetic below.
        let ratio = (equity as f64) / (notional as f64);
        let anchor_f = anchor as f64;
        let zero_price_f = match dir {
            Direction::Long => {
                if ratio >= 1.0 {
                    // Equity exceeds notional — long can never zero
                    // by negative drift alone. Skip.
                    return None;
                }
                anchor_f * (1.0 - ratio)
            }
            Direction::Short => anchor_f * (1.0 + ratio),
        };
        if !zero_price_f.is_finite() || zero_price_f <= 0.0 {
            return None;
        }
        let zero_price = zero_price_f.round() as u64;
        let last = h.last_price as f64;
        // Log-return needed. For long, the price must drop (negative
        // log-return); for short, it must rise (positive).
        let log_ret = (zero_price_f / last).ln();
        let horizon_years = (self.config.horizon_slots as f64)
            / self.config.slots_per_second
            / SECONDS_PER_YEAR;
        if horizon_years <= 0.0 || self.config.annual_vol <= 0.0 {
            return None;
        }
        let sigma_t = self.config.annual_vol * horizon_years.sqrt();
        if sigma_t == 0.0 {
            return None;
        }
        // Two-sided one-touch is bounded above by 2·P(terminal <
        // barrier) for monotonic barriers; we use the terminal-
        // hitting probability as a fast and conservative-enough
        // proxy. For Long, P(touch) ≥ Φ(log_ret/σ_T); for Short,
        // P(touch) ≥ 1 − Φ(log_ret/σ_T).
        let z = log_ret / sigma_t;
        let prob = match dir {
            // Long zero requires log_ret < 0; Φ(z) ∈ (0, 0.5).
            Direction::Long => standard_normal_cdf(z),
            // Short zero requires log_ret > 0; 1 − Φ(z) ∈ (0, 0.5).
            Direction::Short => 1.0 - standard_normal_cdf(z),
        };
        if !prob.is_finite() || prob < self.config.min_probability {
            return None;
        }
        let zero_tick = price_to_tick(
            zero_price,
            self.config.price_tick,
            self.config.tick_aggregation_factor,
        );
        Some(RotatePrediction {
            sub_pool_id: h.sub_pool_id,
            direction: dir,
            zero_price,
            zero_tick,
            probability: prob,
        })
    }
}

/// Floor a price to the nearest aggregation-grid tick. Mirrors the
/// engine's tick-derivation logic:
///
/// `tick = floor(price / price_tick / tick_aggregation_factor)`
///
/// We compute the floored division in u128 to avoid mid-intermediate
/// overflow, then cast back to `i64` (the engine's tick type). The
/// engine treats tick=0 as a valid bucket.
fn price_to_tick(price: u64, price_tick: u64, tick_aggregation_factor: u32) -> i64 {
    let denom = price_tick as u128 * tick_aggregation_factor.max(1) as u128;
    if denom == 0 {
        return 0;
    }
    let t = price as u128 / denom;
    if t > i64::MAX as u128 {
        i64::MAX
    } else {
        t as i64
    }
}

/// Standard-normal CDF via the Abramowitz–Stegun rational
/// approximation (max abs error ≈ 7.5e-8 over the full real line).
/// Pure-Rust, no `libm` dependency, deterministic across host vs
/// BPF compilation.
fn standard_normal_cdf(x: f64) -> f64 {
    // Constants from A&S §26.2.17
    let b1 = 0.319_381_530;
    let b2 = -0.356_563_782;
    let b3 = 1.781_477_937;
    let b4 = -1.821_255_978;
    let b5 = 1.330_274_429;
    let p = 0.231_641_9;
    let abs = x.abs();
    let t = 1.0 / (1.0 + p * abs);
    let pdf = (-(abs * abs) / 2.0).exp() / (2.0 * std::f64::consts::PI).sqrt();
    let series = ((((b5 * t + b4) * t + b3) * t + b2) * t + b1) * t;
    let upper_tail = pdf * series;
    if x >= 0.0 {
        1.0 - upper_tail
    } else {
        upper_tail
    }
}

// =====================================================================
// Wave 9 — Action Executor abstraction
// =====================================================================

/// Wave 9 — outcome of dispatching a single [`KeeperAction`]
/// downstream. Captures the bare minimum a keeper bot needs to log
/// and decide whether to retry, surface to operators, or move on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionDispatchResult {
    /// The executor accepted the action and (in production) submitted
    /// a transaction. The signature is opaque to this crate;
    /// callers stringify it however the underlying RPC layer
    /// formats signatures.
    Submitted {
        /// Optional tx signature. `None` for executors that don't
        /// produce one (e.g. [`DryRunExecutor`]).
        signature: Option<String>,
    },
    /// The executor declined the action without submitting (e.g.
    /// because the on-chain state already satisfies the action).
    /// Caller should drop the action; not a failure.
    Skipped {
        /// Free-form skip rationale.
        reason: &'static str,
    },
    /// The executor failed mid-flight; caller should retry or
    /// surface to operators. The error string is human-readable
    /// only; structured diagnostics live in the Wave 10 RPC client
    /// crate.
    Failed {
        /// Human-readable failure description.
        reason: String,
    },
}

/// Wave 9 — the abstract output channel for a [`Scheduler::plan`]
/// queue. Production keepers bind this to a Solana RPC client; the
/// `chain-mirror` integration tests bind it to a runtime mutator;
/// dry-run benchmarks bind it to [`DryRunExecutor`] which only
/// records, never mutates.
///
/// The trait is deliberately *minimal*: one method per action
/// dispatch, taking owned [`KeeperAction`] values so the executor
/// can move the action into a pending-tx ledger without a borrow
/// check fight at the call site.
///
/// ## Why this is split out from [`Scheduler`]
///
/// `Scheduler` is purely a **planner**. Mixing dispatch into the
/// planner couples retry semantics, transaction-builder details,
/// and RPC error handling to the read-only chain view — all of
/// which evolve at different cadences. Keeping the executor as a
/// trait lets:
///
/// - the Wave 10 RPC keeper bind `solana-client` to it,
/// - tests bind a `chain-mirror` runtime mutator to it,
/// - benchmarks bind [`DryRunExecutor`] for plan-quality
///   measurements,
///
/// all without touching the planner itself.
pub trait ActionExecutor {
    /// Dispatch one keeper action. Implementors decide what happens:
    /// a real executor submits a Solana tx; the dry-run impl appends
    /// to an in-memory queue.
    fn execute(&mut self, action: KeeperAction) -> ActionDispatchResult;
}

/// **Wave 9.** No-side-effect executor that just records actions
/// in-memory. Used by:
///
/// - the Wave 10 RPC keeper's `--dry-run` flag (validate plan
///   without spending CU),
/// - performance benchmarks comparing scheduler tunings,
/// - integration tests that want a baseline of "what would have
///   been submitted" before mutating chain state.
#[derive(Debug, Default, Clone)]
pub struct DryRunExecutor {
    /// Actions recorded in the order they were dispatched.
    pub log: Vec<KeeperAction>,
}

impl DryRunExecutor {
    /// Construct an empty dry-run executor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drain and return the recorded log; the executor is reset to
    /// empty afterwards.
    pub fn drain(&mut self) -> Vec<KeeperAction> {
        std::mem::take(&mut self.log)
    }

    /// Read-only borrow of the log (does not drain).
    pub fn log(&self) -> &[KeeperAction] {
        &self.log
    }
}

impl ActionExecutor for DryRunExecutor {
    fn execute(&mut self, action: KeeperAction) -> ActionDispatchResult {
        self.log.push(action);
        ActionDispatchResult::Submitted { signature: None }
    }
}

/// Convenience helper: drive one full plan→execute cycle. Plans
/// against the given view, dispatches each action through the
/// executor, and returns paired (action, result) tuples in plan
/// order. Wave 10 keeper bots will customise the loop (retry,
/// rate-limiting, gas budgeting); this helper lets tests and
/// dry-runs assert end-to-end behaviour with a single call.
///
/// Errors from [`Scheduler::plan`] are propagated directly; per-
/// action [`ActionDispatchResult::Failed`] outcomes are returned in
/// the result tuple, not as `Err`s, so the caller can decide which
/// failures abort the cycle and which proceed.
pub fn run_plan_cycle(
    scheduler: &mut Scheduler,
    view: &dyn KeeperChainView,
    executor: &mut dyn ActionExecutor,
) -> Result<Vec<(KeeperAction, ActionDispatchResult)>, KeeperError> {
    let actions = scheduler.plan(view)?;
    let mut out = Vec::with_capacity(actions.len());
    for a in actions {
        let r = executor.execute(a);
        out.push((a, r));
    }
    Ok(out)
}

// =====================================================================
// Wave 10 — KeeperLoop state machine
// =====================================================================

/// **Wave 10.** Bag of metrics produced by a single
/// [`KeeperLoop::tick`]. The keeper bot's deployment harness uses
/// this to feed dashboards and alarm thresholds; the data is also
/// the per-tick log line in dry-run mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeeperLoopMetrics {
    /// Number of price samples ingested this tick. Usually 0 or 1
    /// (the env's price source returns at most one fresh sample),
    /// but a backfill scenario may push more.
    pub price_samples_recorded: u32,
    /// Number of init hints emitted by the predictor this tick.
    pub init_hints_recorded: u32,
    /// Number of actions returned by `Scheduler::plan`.
    pub actions_planned: u32,
    /// Subset of `actions_planned` reported as `Submitted`.
    pub actions_submitted: u32,
    /// Subset of `actions_planned` reported as `Failed`.
    pub actions_failed: u32,
    /// Subset of `actions_planned` reported as `Skipped`.
    pub actions_skipped: u32,
    /// Whether the volatility estimator was warm enough that
    /// `apply_to_predictor_config` updated the predictor this tick.
    /// Lets dashboards visualise the auto-tune transition.
    pub vol_estimator_applied: bool,
}

impl KeeperLoopMetrics {
    /// Sum two metric snapshots field-wise. Used by the bot harness
    /// to roll metrics into a per-window aggregate without the
    /// caller writing its own boilerplate.
    pub fn merge(self, other: KeeperLoopMetrics) -> Self {
        Self {
            price_samples_recorded: self.price_samples_recorded + other.price_samples_recorded,
            init_hints_recorded: self.init_hints_recorded + other.init_hints_recorded,
            actions_planned: self.actions_planned + other.actions_planned,
            actions_submitted: self.actions_submitted + other.actions_submitted,
            actions_failed: self.actions_failed + other.actions_failed,
            actions_skipped: self.actions_skipped + other.actions_skipped,
            vol_estimator_applied: self.vol_estimator_applied || other.vol_estimator_applied,
        }
    }
}

/// **Wave 10.** Per-tick outcome of the keeper loop. Wraps
/// [`KeeperLoopMetrics`] alongside the (action, result) pairs
/// `run_plan_cycle` produced so callers retain full visibility for
/// post-mortem inspection.
#[derive(Debug, Clone)]
pub struct KeeperLoopOutcome {
    /// Aggregate metrics for the tick.
    pub metrics: KeeperLoopMetrics,
    /// Per-action (action, dispatch-result) pairs in plan order —
    /// same shape as `run_plan_cycle`'s return value.
    pub dispatched: Vec<(KeeperAction, ActionDispatchResult)>,
}

/// **Wave 10.** External-world trait the keeper bot consumes. Plug-
/// gable so the same `KeeperLoop` runs against:
///
/// 1. `chain-mirror` for end-to-end host tests (this crate's
///    integration suite),
/// 2. a Solana RPC adapter for production (wave 11),
/// 3. an event-replay harness for offline keeper-policy backtests.
///
/// Each trait method is intentionally synchronous — async wrappers
/// can wrap a synchronous `KeeperBotEnvironment` without leaking
/// `tokio` into this crate's deps.
pub trait KeeperBotEnvironment {
    /// Read-only chain view for this tick. Must reflect chain state
    /// at the *moment of the call*, not a stale cache; freshness is
    /// the env's responsibility.
    fn chain_view(&self) -> &dyn KeeperChainView;
    /// Best-effort fresh oracle sample. `None` if no new price has
    /// been observed since the last call (the keeper bot polls more
    /// often than the oracle updates) — the loop silently skips
    /// ingest in that case.
    fn fetch_price_sample(&mut self) -> Option<PriceSample>;
    /// Mutable executor — must outlive the loop's tick scope.
    fn executor(&mut self) -> &mut dyn ActionExecutor;
}

/// **Wave 10.** Tunable knobs for [`KeeperLoop::tick`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeeperLoopConfig {
    /// Whether the loop should drive the rotate-risk predictor on
    /// every tick. Default `true`. Set `false` only when the bot is
    /// running in a "drain only" maintenance mode (e.g. while a
    /// predictor bug is being investigated) — pre-syncs and closes
    /// keep working unaffected.
    pub run_predictor: bool,
    /// Whether the loop should auto-tune
    /// [`PredictorConfig::annual_vol`] from the
    /// [`RealizedVolatilityEstimator`]. Default `true`. Off at
    /// startup if you want to lock the predictor to a hand-set
    /// volatility.
    pub auto_tune_vol: bool,
}

impl Default for KeeperLoopConfig {
    fn default() -> Self {
        Self {
            run_predictor: true,
            auto_tune_vol: true,
        }
    }
}

/// **Wave 10.** End-to-end keeper state machine. One [`KeeperLoop`]
/// instance owns the long-lived state (vol estimator history,
/// scheduler init-hint set, predictor config); a deployment harness
/// drives [`KeeperLoop::tick`] at whatever cadence the operator
/// chooses (typically every 1–5 slots).
///
/// The loop is intentionally **not** an async runtime; it has no
/// `Future`s, no `tokio` dependency, no clock. The deployment
/// harness owns the cadence. This keeps the loop testable on
/// `chain-mirror` without spawning a runtime and lets the same code
/// power a CI integration test, a backtest replay, and a production
/// bot.
///
/// ## Tick lifecycle
///
/// ```text
/// 1. env.fetch_price_sample()  → vol_estimator.record(...)?
/// 2. vol_estimator.apply_to_predictor_config(predictor)?    // auto-tune
/// 3. predictor.populate_scheduler(view, scheduler)          // init hints
/// 4. run_plan_cycle(scheduler, view, executor)              // plan + dispatch
/// 5. Bag everything into KeeperLoopOutcome and return.
/// ```
///
/// All five steps execute every tick — the loop is intentionally
/// stateless across ticks except for the long-lived components it
/// owns.
#[derive(Debug)]
pub struct KeeperLoop {
    config: KeeperLoopConfig,
    scheduler: Scheduler,
    predictor: RotateRiskPredictor,
    vol_estimator: RealizedVolatilityEstimator,
}

impl KeeperLoop {
    /// Construct a fresh loop with the given long-lived components.
    pub fn new(
        config: KeeperLoopConfig,
        scheduler: Scheduler,
        predictor: RotateRiskPredictor,
        vol_estimator: RealizedVolatilityEstimator,
    ) -> Self {
        Self {
            config,
            scheduler,
            predictor,
            vol_estimator,
        }
    }

    /// Read-only borrow of the loop's active config.
    pub fn config(&self) -> &KeeperLoopConfig {
        &self.config
    }

    /// Replace the active config.
    pub fn set_config(&mut self, config: KeeperLoopConfig) {
        self.config = config;
    }

    /// Read-only borrow of the loop's scheduler. Useful for tests
    /// that want to inspect pending init hints between ticks.
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// Mutable borrow of the loop's scheduler. Caller can manually
    /// `record_init_hint` / `cancel_init_hint` between ticks for
    /// scenarios the predictor doesn't cover.
    pub fn scheduler_mut(&mut self) -> &mut Scheduler {
        &mut self.scheduler
    }

    /// Read-only borrow of the volatility estimator.
    pub fn vol_estimator(&self) -> &RealizedVolatilityEstimator {
        &self.vol_estimator
    }

    /// Read-only borrow of the rotate-risk predictor.
    pub fn predictor(&self) -> &RotateRiskPredictor {
        &self.predictor
    }

    /// One pass of the keeper loop. See type-level doc-comment for
    /// the full lifecycle. Errors propagate from
    /// `Scheduler::plan` (i.e. detected off-chain reconstruction
    /// invariant violations); per-action dispatch failures are
    /// captured in `KeeperLoopOutcome::dispatched`, not the `Err`
    /// channel — the loop never aborts because of a single failed
    /// transaction.
    pub fn tick<E: KeeperBotEnvironment + ?Sized>(
        &mut self,
        env: &mut E,
    ) -> Result<KeeperLoopOutcome, KeeperError> {
        let mut metrics = KeeperLoopMetrics::default();

        // Step 1 — record any fresh oracle sample.
        if let Some(sample) = env.fetch_price_sample() {
            self.vol_estimator.record(sample);
            metrics.price_samples_recorded = 1;
        }

        // Step 2 — auto-tune the predictor if the estimator is warm.
        if self.config.auto_tune_vol {
            // Snapshot, mutate, write-back to keep `predictor` API a
            // single Cell of truth.
            let mut cfg = *self.predictor.config();
            metrics.vol_estimator_applied =
                self.vol_estimator.apply_to_predictor_config(&mut cfg);
            self.predictor.set_config(cfg);
        }

        // Step 3 — predictor pushes init hints into the scheduler.
        // The chain-view immutable borrow ends at the end of this
        // statement so step 4 can take a mutable borrow on `env`
        // for the executor.
        if self.config.run_predictor {
            let preds = self
                .predictor
                .populate_scheduler(env.chain_view(), &mut self.scheduler);
            metrics.init_hints_recorded = preds.len() as u32;
        }

        // Step 4 — plan + dispatch. We inline `run_plan_cycle`'s
        // body here so the chain-view immutable borrow ends after
        // `Scheduler::plan` (which is the only thing that needs
        // it), freeing `env` for the executor borrow that follows.
        // Plan errors abort; dispatch failures are recorded in
        // `metrics` and `outcome.dispatched`.
        let actions = self.scheduler.plan(env.chain_view())?;
        metrics.actions_planned = actions.len() as u32;
        let mut dispatched: Vec<(KeeperAction, ActionDispatchResult)> =
            Vec::with_capacity(actions.len());
        for action in actions {
            let result = env.executor().execute(action);
            match &result {
                ActionDispatchResult::Submitted { .. } => metrics.actions_submitted += 1,
                ActionDispatchResult::Failed { .. } => metrics.actions_failed += 1,
                ActionDispatchResult::Skipped { .. } => metrics.actions_skipped += 1,
            }
            dispatched.push((action, result));
        }

        Ok(KeeperLoopOutcome {
            metrics,
            dispatched,
        })
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub chain view for unit testing — just stores everything in
    /// vecs/maps.
    struct StubView {
        sub_pool_ids: Vec<u32>,
        buckets: HashMap<u32, Vec<BucketSnapshot>>,
        ledgers: HashMap<(u32, Direction), LedgerSnapshot>,
    }

    impl StubView {
        fn new(sub_pool_ids: Vec<u32>) -> Self {
            Self {
                sub_pool_ids,
                buckets: HashMap::new(),
                ledgers: HashMap::new(),
            }
        }
    }

    impl KeeperChainView for StubView {
        fn sub_pool_ids(&self) -> Vec<u32> {
            self.sub_pool_ids.clone()
        }
        fn buckets(&self, sub_pool_id: u32) -> Vec<BucketSnapshot> {
            self.buckets.get(&sub_pool_id).cloned().unwrap_or_default()
        }
        fn ledger(&self, sub_pool_id: u32, direction: Direction) -> Option<LedgerSnapshot> {
            self.ledgers.get(&(sub_pool_id, direction)).copied()
        }
    }

    fn live_bucket(sub: u32, dir: Direction, tick: i64, last_applied: u64) -> BucketSnapshot {
        BucketSnapshot {
            sub_pool_id: sub,
            direction: dir,
            tick,
            anchor_price: 100,
            total_recovery_shares: 1_000,
            total_recovery_notional: 1_000_000,
            accrued_value: 0,
            position_count: 1,
            last_applied_index: last_applied,
        }
    }

    fn dead_bucket(sub: u32, dir: Direction, tick: i64, last_applied: u64) -> BucketSnapshot {
        BucketSnapshot {
            sub_pool_id: sub,
            direction: dir,
            tick,
            anchor_price: 100,
            total_recovery_shares: 0,
            total_recovery_notional: 0,
            accrued_value: 0,
            position_count: 0,
            last_applied_index: last_applied,
        }
    }

    fn ledger(sub: u32, dir: Direction, head: u64) -> LedgerSnapshot {
        LedgerSnapshot {
            sub_pool_id: sub,
            direction: dir,
            next_event_index: head,
            bucket_count_hint: 0,
        }
    }

    #[test]
    fn empty_view_produces_empty_plan() {
        let view = StubView::new(vec![]);
        let mut sched = Scheduler::default();
        assert!(sched.plan(&view).unwrap().is_empty());
    }

    #[test]
    fn live_bucket_with_pending_emits_pre_sync() {
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(0, vec![live_bucket(0, Direction::Long, 5, 10)]);
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 13));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::default();
        let actions = sched.plan(&view).unwrap();
        assert_eq!(actions.len(), 1);
        match actions[0] {
            KeeperAction::PreSyncDormantBucket { tick, pending, .. } => {
                assert_eq!(tick, 5);
                assert_eq!(pending, 3);
            }
            _ => panic!("wrong action: {:?}", actions[0]),
        }
    }

    #[test]
    fn dead_caught_up_bucket_emits_close() {
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(0, vec![dead_bucket(0, Direction::Long, 5, 10)]);
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 10));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::default();
        let actions = sched.plan(&view).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            actions[0],
            KeeperAction::CloseDormantBucket { tick: 5, .. }
        ));
    }

    #[test]
    fn dead_bucket_with_pending_emits_pre_sync_first_not_close() {
        // A dead bucket that hasn't caught up yet must NOT be closed
        // — close requires `last_applied_index >= next_event_index`.
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(0, vec![dead_bucket(0, Direction::Long, 5, 7)]);
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 10));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::default();
        let actions = sched.plan(&view).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], KeeperAction::PreSyncDormantBucket { .. }));
    }

    #[test]
    fn priority_orders_init_first_then_pre_sync_then_close() {
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(
            0,
            vec![
                live_bucket(0, Direction::Long, 5, 0),  // PreSync, pending = 100
                dead_bucket(0, Direction::Long, 6, 100), // Close
            ],
        );
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 100));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::default();
        sched.record_init_hint(0, Direction::Long, 9, InitRationale::Explicit);
        let actions = sched.plan(&view).unwrap();
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], KeeperAction::InitDormantBucket { .. }));
        assert!(matches!(actions[1], KeeperAction::PreSyncDormantBucket { .. }));
        assert!(matches!(actions[2], KeeperAction::CloseDormantBucket { .. }));
    }

    #[test]
    fn pre_sync_priority_scales_with_pending_depth() {
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(
            0,
            vec![
                live_bucket(0, Direction::Long, 5, 0),   // pending=100
                live_bucket(0, Direction::Long, 6, 90),  // pending=10
            ],
        );
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 100));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::default();
        let actions = sched.plan(&view).unwrap();
        assert_eq!(actions.len(), 2);
        match (&actions[0], &actions[1]) {
            (
                KeeperAction::PreSyncDormantBucket { tick: t0, pending: p0, .. },
                KeeperAction::PreSyncDormantBucket { tick: t1, pending: p1, .. },
            ) => {
                assert!(p0 > p1, "more-behind bucket runs first");
                assert_eq!(*t0, 5);
                assert_eq!(*t1, 6);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn min_pending_threshold_filters_out_small_lag() {
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(0, vec![live_bucket(0, Direction::Long, 5, 9)]);
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 10));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::new(SchedulerConfig {
            min_pending_for_pre_sync: 5,
            ..Default::default()
        });
        assert!(sched.plan(&view).unwrap().is_empty(),
            "lag of 1 below threshold of 5 must be silenced");
    }

    #[test]
    fn close_dead_buckets_can_be_disabled() {
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(0, vec![dead_bucket(0, Direction::Long, 5, 10)]);
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 10));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::new(SchedulerConfig {
            close_dead_buckets: false,
            ..Default::default()
        });
        assert!(sched.plan(&view).unwrap().is_empty());
    }

    #[test]
    fn max_actions_per_plan_caps_queue_length() {
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(
            0,
            (0..10).map(|t| live_bucket(0, Direction::Long, t, 0)).collect(),
        );
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 100));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::new(SchedulerConfig {
            max_actions_per_plan: 3,
            ..Default::default()
        });
        assert_eq!(sched.plan(&view).unwrap().len(), 3);
    }

    #[test]
    fn init_hint_for_existing_pda_is_dropped() {
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(0, vec![dead_bucket(0, Direction::Long, 5, 0)]);
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 0));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));

        let mut sched = Scheduler::default();
        sched.record_init_hint(0, Direction::Long, 5, InitRationale::Explicit);
        let actions = sched.plan(&view).unwrap();
        // Hint dropped (PDA already exists). Bucket is also dead +
        // caught-up so a Close is emitted.
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], KeeperAction::CloseDormantBucket { .. }));
        assert_eq!(sched.pending_init_hint_count(), 1, "hint stays until consumed");
    }

    #[test]
    fn init_hint_is_one_shot_after_emit() {
        // Hint becomes an Init action and then drains so the next
        // plan call returns nothing if the chain view hasn't been
        // updated. Caller is responsible for re-recording on failure.
        let mut view = StubView::new(vec![0]);
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 0));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));

        let mut sched = Scheduler::default();
        sched.record_init_hint(0, Direction::Long, 9, InitRationale::RotateRiskHorizon);
        let first = sched.plan(&view).unwrap();
        assert_eq!(first.len(), 1);
        let second = sched.plan(&view).unwrap();
        assert!(second.is_empty(), "drain on emit");
    }

    #[test]
    fn bucket_ahead_of_ledger_is_an_invariant_violation() {
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(0, vec![live_bucket(0, Direction::Long, 5, 100)]);
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 50));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::default();
        let err = sched.plan(&view).unwrap_err();
        assert!(matches!(err, KeeperError::BucketAheadOfLedger { applied: 100, head: 50, .. }));
    }

    // ===============================================================
    // Wave 9 — RotateRiskPredictor
    // ===============================================================

    fn health(
        sub: u32,
        last_price: u64,
        long_eq: u128,
        short_eq: u128,
        long_notional: u128,
        short_notional: u128,
    ) -> SubPoolHealth {
        SubPoolHealth {
            sub_pool_id: sub,
            last_price,
            long_anchor_price: last_price,
            short_anchor_price: last_price,
            long_pool_equity: long_eq,
            short_pool_equity: short_eq,
            long_active_notional: long_notional,
            short_active_notional: short_notional,
            long_active_generation: 1,
            short_active_generation: 1,
        }
    }

    /// Standard normal CDF anchors at known values. Establishes the
    /// approximation is faithful enough that the rest of the
    /// predictor logic is grounded in correct probability math.
    #[test]
    fn standard_normal_cdf_anchors() {
        let phi_0 = standard_normal_cdf(0.0);
        assert!((phi_0 - 0.5).abs() < 1e-6);
        // Φ(1.96) ≈ 0.975
        let phi_196 = standard_normal_cdf(1.96);
        assert!((phi_196 - 0.975).abs() < 1e-3);
        // Φ(-2.0) ≈ 0.02275
        let phi_neg2 = standard_normal_cdf(-2.0);
        assert!((phi_neg2 - 0.022_75).abs() < 1e-3);
        // Tail saturation
        assert!(standard_normal_cdf(10.0) > 0.999_999);
        assert!(standard_normal_cdf(-10.0) < 1e-6);
    }

    /// `price_to_tick` floors against the aggregation grid. Vital
    /// because emitting a hint at the *wrong* tick wastes a PDA
    /// without unblocking the rotate.
    #[test]
    fn price_to_tick_floors_against_aggregation_grid() {
        // price 99, tick 1, agg 10 → 99 / 10 = 9
        assert_eq!(price_to_tick(99, 1, 10), 9);
        // price 100, tick 1, agg 10 → 100 / 10 = 10
        assert_eq!(price_to_tick(100, 1, 10), 10);
        // price 100, tick 5, agg 4 → 100 / 20 = 5
        assert_eq!(price_to_tick(100, 5, 4), 5);
        // price 0 → tick 0
        assert_eq!(price_to_tick(0, 1, 1), 0);
        // saturate at i64::MAX for wildly-overflowing prices.
        // price = u64::MAX, tick = 1, agg = 1 → u128 ≈ 1.8e19 > i64::MAX
        assert_eq!(price_to_tick(u64::MAX, 1, 1), i64::MAX);
    }

    /// Long pool with vanishing equity ratio (1%) under high vol +
    /// long horizon must surface. Headroom of 1% vs σ_T ~ 4% is z ≈
    /// −0.25, prob ≈ 40%, well above min 5%.
    #[test]
    fn predictor_surfaces_imminent_long_zero() {
        let predictor = RotateRiskPredictor::new(PredictorConfig {
            annual_vol: 1.0,            // 100% annualised
            horizon_slots: 1_000_000,   // generous so σ_T is large
            slots_per_second: 2.5,
            min_probability: 0.01,
            tick_aggregation_factor: 1,
            price_tick: 1,
        });
        // long equity 100 vs notional 10_000 → ratio 1%; zero ≈ 99% of last
        let h = health(7, 100, 100, 10_000, 10_000, 10_000);
        let preds = predictor.predict_one(&h);
        let long = preds.iter().find(|p| p.direction == Direction::Long).unwrap();
        assert_eq!(long.sub_pool_id, 7);
        assert!(long.zero_price < h.last_price, "long zero must be below current price");
        assert!(long.probability > 0.01);
    }

    /// Long pool with healthy equity ratio (≥ 100%) MUST be filtered
    /// out — the directional pool can never zero from negative drift
    /// alone, so emitting an init hint would be pure noise.
    #[test]
    fn predictor_skips_overcollateralised_long_pool() {
        let predictor = RotateRiskPredictor::new(PredictorConfig::default());
        // long equity == notional → ratio 100% → predictor returns None
        let h = health(0, 100, 10_000, 1_000, 10_000, 10_000);
        let preds = predictor.predict_one(&h);
        assert!(preds.iter().all(|p| p.direction != Direction::Long));
    }

    /// Sub pool with no positions (notional == 0) MUST be filtered
    /// out for both directions — there is nothing to rotate.
    #[test]
    fn predictor_skips_empty_pool() {
        let predictor = RotateRiskPredictor::new(PredictorConfig::default());
        let h = health(0, 100, 0, 0, 0, 0);
        assert!(predictor.predict_one(&h).is_empty());
    }

    /// `populate_scheduler` must drive [`Scheduler::record_init_hint`]
    /// for every prediction. After populating, calling
    /// [`Scheduler::plan`] must surface the corresponding
    /// `InitDormantBucket` actions.
    #[test]
    fn populate_scheduler_drives_init_hints_into_plan() {
        struct HealthView {
            health: SubPoolHealth,
        }
        impl KeeperChainView for HealthView {
            fn sub_pool_ids(&self) -> Vec<u32> {
                vec![self.health.sub_pool_id]
            }
            fn buckets(&self, _sub_pool_id: u32) -> Vec<BucketSnapshot> {
                vec![]
            }
            fn ledger(&self, sub_pool_id: u32, direction: Direction) -> Option<LedgerSnapshot> {
                Some(LedgerSnapshot {
                    sub_pool_id,
                    direction,
                    next_event_index: 0,
                    bucket_count_hint: 0,
                })
            }
            fn sub_pool_health(&self, sub_pool_id: u32) -> Option<SubPoolHealth> {
                if sub_pool_id == self.health.sub_pool_id {
                    Some(self.health)
                } else {
                    None
                }
            }
        }
        let predictor = RotateRiskPredictor::new(PredictorConfig {
            annual_vol: 1.0,
            horizon_slots: 1_000_000,
            slots_per_second: 2.5,
            min_probability: 0.01,
            tick_aggregation_factor: 1,
            price_tick: 1,
        });
        let view = HealthView {
            health: health(7, 100, 100, 100, 10_000, 10_000),
        };
        let mut sched = Scheduler::default();
        let preds = predictor.populate_scheduler(&view, &mut sched);
        assert!(!preds.is_empty(), "predictor must surface at least one tick");
        assert_eq!(sched.pending_init_hint_count(), preds.len());

        let actions = sched.plan(&view).unwrap();
        let init_count = actions
            .iter()
            .filter(|a| matches!(a, KeeperAction::InitDormantBucket { rationale, .. } if *rationale == InitRationale::RotateRiskHorizon))
            .count();
        assert_eq!(
            init_count, preds.len(),
            "every prediction must surface as an Init action"
        );
        // Hints drained after emit.
        assert_eq!(sched.pending_init_hint_count(), 0);
    }

    /// Sub-pools without health data MUST be silently skipped — the
    /// default `KeeperChainView::sub_pool_health` returns `None`,
    /// which lets older view implementations integrate gradually.
    #[test]
    fn populate_scheduler_no_health_silently_skips() {
        let view = StubView::new(vec![0]);
        let predictor = RotateRiskPredictor::new(PredictorConfig::default());
        let mut sched = Scheduler::default();
        let preds = predictor.populate_scheduler(&view, &mut sched);
        assert!(preds.is_empty());
        assert_eq!(sched.pending_init_hint_count(), 0);
    }

    // ===============================================================
    // Wave 9 — ActionExecutor / run_plan_cycle
    // ===============================================================

    /// `DryRunExecutor` records every dispatched action, returns
    /// `Submitted { signature: None }`, and lets the caller drain
    /// the log between cycles.
    #[test]
    fn dry_run_executor_records_each_action() {
        let mut exec = DryRunExecutor::new();
        let r1 = exec.execute(KeeperAction::CloseDormantBucket {
            sub_pool_id: 0,
            direction: Direction::Long,
            tick: 5,
        });
        assert_eq!(r1, ActionDispatchResult::Submitted { signature: None });
        let r2 = exec.execute(KeeperAction::PreSyncDormantBucket {
            sub_pool_id: 0,
            direction: Direction::Long,
            tick: 5,
            pending: 3,
        });
        assert_eq!(r2, ActionDispatchResult::Submitted { signature: None });
        assert_eq!(exec.log().len(), 2);

        let drained = exec.drain();
        assert_eq!(drained.len(), 2);
        assert!(exec.log().is_empty());
    }

    /// `run_plan_cycle` runs the planner, dispatches each action
    /// through the executor, and returns the (action, result) pairs
    /// in plan order. The dry-run executor's log MUST equal the
    /// dispatched action sequence.
    #[test]
    fn run_plan_cycle_links_planner_and_executor() {
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(
            0,
            vec![
                live_bucket(0, Direction::Long, 5, 0),
                dead_bucket(0, Direction::Long, 6, 100),
            ],
        );
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 100));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::default();
        sched.record_init_hint(0, Direction::Long, 9, InitRationale::Explicit);
        let mut exec = DryRunExecutor::new();
        let pairs = run_plan_cycle(&mut sched, &view, &mut exec).unwrap();

        assert_eq!(pairs.len(), 3);
        // Plan order is priority-descending: Init, PreSync, Close.
        assert!(matches!(pairs[0].0, KeeperAction::InitDormantBucket { .. }));
        assert!(matches!(pairs[1].0, KeeperAction::PreSyncDormantBucket { .. }));
        assert!(matches!(pairs[2].0, KeeperAction::CloseDormantBucket { .. }));
        for (_, r) in &pairs {
            assert_eq!(*r, ActionDispatchResult::Submitted { signature: None });
        }
        // Executor log mirrors plan order.
        let log = exec.log();
        assert_eq!(log.len(), 3);
        for i in 0..3 {
            assert_eq!(pairs[i].0, log[i]);
        }
    }

    /// A custom executor that simulates an RPC failure on the first
    /// `PreSync` and succeeds otherwise. `run_plan_cycle` must
    /// surface the failure result without aborting the cycle so the
    /// caller can decide retry semantics — this contract is the
    /// reason `run_plan_cycle` returns per-action results rather
    /// than a single `Result`.
    #[test]
    fn run_plan_cycle_surfaces_per_action_failures_without_aborting() {
        struct FlakyExec {
            seen_pre_sync: bool,
        }
        impl ActionExecutor for FlakyExec {
            fn execute(&mut self, action: KeeperAction) -> ActionDispatchResult {
                match action {
                    KeeperAction::PreSyncDormantBucket { .. } if !self.seen_pre_sync => {
                        self.seen_pre_sync = true;
                        ActionDispatchResult::Failed {
                            reason: "rpc 503".to_string(),
                        }
                    }
                    _ => ActionDispatchResult::Submitted {
                        signature: Some("sig".into()),
                    },
                }
            }
        }
        let mut view = StubView::new(vec![0]);
        view.buckets.insert(
            0,
            vec![
                live_bucket(0, Direction::Long, 5, 0),
                live_bucket(0, Direction::Long, 6, 0),
            ],
        );
        view.ledgers.insert((0, Direction::Long), ledger(0, Direction::Long, 50));
        view.ledgers.insert((0, Direction::Short), ledger(0, Direction::Short, 0));
        let mut sched = Scheduler::default();
        let mut exec = FlakyExec { seen_pre_sync: false };
        let pairs = run_plan_cycle(&mut sched, &view, &mut exec).unwrap();
        assert_eq!(pairs.len(), 2);
        // First action failed, second succeeded — the cycle didn't abort.
        assert!(matches!(pairs[0].1, ActionDispatchResult::Failed { .. }));
        assert!(matches!(pairs[1].1, ActionDispatchResult::Submitted { .. }));
    }

    // ===============================================================
    // Wave 10 — RealizedVolatilityEstimator
    // ===============================================================

    fn vol_config(min_samples: usize) -> RealizedVolatilityEstimatorConfig {
        RealizedVolatilityEstimatorConfig {
            max_samples: 1024,
            max_age_slots: u64::MAX,
            min_samples,
            slots_per_second: 2.5,
            min_clamp: 1e-6,
            max_clamp: 1e6,
        }
    }

    /// Below `min_samples` the estimator returns `None`; this is
    /// the contract that lets the keeper bot keep its hand-tuned
    /// default during the boot window.
    #[test]
    fn vol_estimator_none_until_warmup() {
        let mut est = RealizedVolatilityEstimator::new(vol_config(5));
        for slot in 0..4 {
            est.record(PriceSample { price: 100, slot });
        }
        assert!(est.current_estimate().is_none());
        assert_eq!(est.sample_count(), 4);
    }

    /// Constant prices yield zero variance → σ̂ clamps to
    /// `min_clamp` rather than 0.0 (avoids predictor divide-by-zero
    /// on a degenerate vol input).
    #[test]
    fn vol_estimator_constant_price_clamps_to_floor() {
        let mut cfg = vol_config(3);
        cfg.min_clamp = 0.05;
        let mut est = RealizedVolatilityEstimator::new(cfg);
        for slot in 0..16 {
            est.record(PriceSample { price: 100, slot: slot * 10 });
        }
        let sigma = est.current_estimate().expect("warm");
        assert!((sigma - 0.05).abs() < 1e-9, "expected floor clamp, got {sigma}");
    }

    /// A geometric-Brownian-motion-ish synthetic walk with a fixed
    /// per-step log-return std of `0.01` (≈1% per slot interval at
    /// 1 sec/slot) at slots_per_second=1 produces an annualised σ
    /// close to `0.01 * sqrt(seconds_per_year)` ≈ 56.2. Exact value
    /// is implementation-dependent due to ln-grid aliasing on integer
    /// prices; assert order-of-magnitude.
    #[test]
    fn vol_estimator_synthetic_walk_recovers_order_of_magnitude() {
        let cfg = RealizedVolatilityEstimatorConfig {
            max_samples: 1024,
            max_age_slots: u64::MAX,
            min_samples: 50,
            slots_per_second: 1.0,
            min_clamp: 0.001,
            max_clamp: 1e9,
        };
        let mut est = RealizedVolatilityEstimator::new(cfg);
        // Simulate r ~ const = 0.01 per unit slot time. Each slot
        // multiplies price by exp(0.01) ≈ 1.01005.
        let mut p: f64 = 1_000_000.0;
        for slot in 0..200 {
            est.record(PriceSample {
                price: p as u64,
                slot,
            });
            p *= (0.01f64).exp();
        }
        let sigma = est.current_estimate().expect("warm");
        // At slots_per_second=1, dt_years per step = 1/SECONDS_PER_YEAR.
        // sigma_annual = sqrt(r^2 * total_steps / total_dt_years)
        //              = |r| * sqrt(SECONDS_PER_YEAR)
        let expected = 0.01f64 * (365.0 * 24.0 * 3600.0_f64).sqrt();
        // Allow 15% tolerance for integer-price aliasing.
        let ratio = sigma / expected;
        assert!(
            (0.85..=1.15).contains(&ratio),
            "sigma={sigma} expected≈{expected}, ratio={ratio}"
        );
    }

    /// Out-of-order or duplicate slots MUST be silently dropped —
    /// the keeper bot polls async and may see RPC reordering.
    #[test]
    fn vol_estimator_drops_out_of_order_and_duplicate_slots() {
        let mut est = RealizedVolatilityEstimator::new(vol_config(2));
        est.record(PriceSample { price: 100, slot: 10 });
        est.record(PriceSample { price: 200, slot: 5 }); // out-of-order
        est.record(PriceSample { price: 200, slot: 10 }); // duplicate
        est.record(PriceSample { price: 0, slot: 11 }); // zero price
        est.record(PriceSample { price: 105, slot: 11 });
        assert_eq!(est.sample_count(), 2);
    }

    /// `max_samples` and `max_age_slots` evict — whichever bites
    /// first. We test both axes.
    #[test]
    fn vol_estimator_evicts_by_count_then_by_age() {
        let mut est = RealizedVolatilityEstimator::new(RealizedVolatilityEstimatorConfig {
            max_samples: 4,
            max_age_slots: u64::MAX,
            min_samples: 2,
            slots_per_second: 2.5,
            min_clamp: 0.01,
            max_clamp: 100.0,
        });
        for slot in 0u64..10 {
            est.record(PriceSample { price: 100 + slot, slot });
        }
        assert_eq!(est.sample_count(), 4);

        let mut est = RealizedVolatilityEstimator::new(RealizedVolatilityEstimatorConfig {
            max_samples: 1024,
            max_age_slots: 5,
            min_samples: 2,
            slots_per_second: 2.5,
            min_clamp: 0.01,
            max_clamp: 100.0,
        });
        est.record(PriceSample { price: 100, slot: 0 });
        est.record(PriceSample { price: 101, slot: 1 });
        est.record(PriceSample { price: 102, slot: 2 });
        est.record(PriceSample { price: 103, slot: 100 });
        // Now slot 100, cutoff = 100 - 5 = 95; samples at 0/1/2 evicted.
        assert_eq!(est.sample_count(), 1);
    }

    /// `apply_to_predictor_config` is the keeper bot's per-tick
    /// hook: when the estimator is warm, the predictor's
    /// `annual_vol` is over-written; when it isn't, the caller's
    /// hand-tuned default survives.
    #[test]
    fn vol_estimator_apply_overwrites_only_when_warm() {
        let mut est = RealizedVolatilityEstimator::new(vol_config(5));
        let mut cfg = PredictorConfig::default();
        let original = cfg.annual_vol;
        assert!(!est.apply_to_predictor_config(&mut cfg));
        assert_eq!(cfg.annual_vol, original, "no overwrite below min_samples");
        for slot in 0u64..16 {
            est.record(PriceSample {
                price: 100 + slot * 10,
                slot: slot * 10,
            });
        }
        assert!(est.apply_to_predictor_config(&mut cfg));
        assert!(cfg.annual_vol > 0.0);
        assert_ne!(cfg.annual_vol, original, "warm estimate must overwrite");
    }

    /// `reset` drops every retained sample — the keeper bot calls
    /// this on market-pause transitions so the post-resume window
    /// doesn't blend regimes.
    #[test]
    fn vol_estimator_reset_clears_history() {
        let mut est = RealizedVolatilityEstimator::new(vol_config(2));
        est.record(PriceSample { price: 100, slot: 0 });
        est.record(PriceSample { price: 110, slot: 10 });
        assert!(est.current_estimate().is_some());
        est.reset();
        assert_eq!(est.sample_count(), 0);
        assert!(est.current_estimate().is_none());
    }

    // -----------------------------------------------------------------
    // Wave 10 — KeeperLoop state machine
    // -----------------------------------------------------------------

    /// Tiny env that hands out a stub view, an injectable price-
    /// sample source (FIFO queue), and a `DryRunExecutor`.
    struct StubBotEnv {
        view: StubView,
        feed: std::collections::VecDeque<PriceSample>,
        executor: DryRunExecutor,
    }

    impl StubBotEnv {
        fn new(view: StubView) -> Self {
            Self {
                view,
                feed: std::collections::VecDeque::new(),
                executor: DryRunExecutor::new(),
            }
        }
        fn push_sample(&mut self, sample: PriceSample) {
            self.feed.push_back(sample);
        }
    }

    impl KeeperBotEnvironment for StubBotEnv {
        fn chain_view(&self) -> &dyn KeeperChainView {
            &self.view
        }
        fn fetch_price_sample(&mut self) -> Option<PriceSample> {
            self.feed.pop_front()
        }
        fn executor(&mut self) -> &mut dyn ActionExecutor {
            &mut self.executor
        }
    }

    fn vol_config_with(min: usize, slots_per_second: f64) -> RealizedVolatilityEstimatorConfig {
        RealizedVolatilityEstimatorConfig {
            min_samples: min,
            max_samples: 256,
            max_age_slots: 100_000,
            slots_per_second,
            min_clamp: 0.001,
            max_clamp: 50.0,
        }
    }

    /// Empty pool, no oracle samples — the loop ticks cleanly with
    /// zero actions, zero metrics, no panics.
    #[test]
    fn keeper_loop_idle_tick_produces_zero_actions() {
        let mut env = StubBotEnv::new(StubView::new(vec![]));
        let mut keeper = KeeperLoop::new(
            KeeperLoopConfig::default(),
            Scheduler::new(SchedulerConfig::default()),
            RotateRiskPredictor::new(PredictorConfig::default()),
            RealizedVolatilityEstimator::new(vol_config_with(5, 2.5)),
        );
        let outcome = keeper.tick(&mut env).expect("idle tick succeeds");
        assert_eq!(outcome.metrics, KeeperLoopMetrics::default());
        assert!(outcome.dispatched.is_empty());
    }

    /// One init hint already pending → exactly one `InitDormantBucket`
    /// action lands on the executor, metrics line up, executor log
    /// matches the dispatched sequence.
    #[test]
    fn keeper_loop_dispatches_pending_init_hint() {
        let view = StubView::new(vec![0]);
        let mut env = StubBotEnv::new(view);
        let mut keeper = KeeperLoop::new(
            KeeperLoopConfig {
                run_predictor: false,
                auto_tune_vol: false,
            },
            Scheduler::new(SchedulerConfig::default()),
            RotateRiskPredictor::new(PredictorConfig::default()),
            RealizedVolatilityEstimator::new(vol_config_with(5, 2.5)),
        );
        keeper.scheduler_mut().record_init_hint(
            0,
            Direction::Long,
            7,
            InitRationale::Explicit,
        );

        let outcome = keeper.tick(&mut env).expect("tick");
        assert_eq!(outcome.metrics.actions_planned, 1);
        assert_eq!(outcome.metrics.actions_submitted, 1);
        assert_eq!(outcome.metrics.actions_failed, 0);
        assert_eq!(outcome.metrics.actions_skipped, 0);
        assert_eq!(outcome.metrics.init_hints_recorded, 0);
        assert_eq!(outcome.dispatched.len(), 1);
        assert!(matches!(
            outcome.dispatched[0].0,
            KeeperAction::InitDormantBucket { sub_pool_id: 0, direction: Direction::Long, tick: 7, .. }
        ));
        assert_eq!(env.executor.log().len(), 1);
    }

    /// `auto_tune_vol = true` overrides the predictor's `annual_vol`
    /// once the estimator is warm. Wave-9 hand-set σ → wave-10
    /// σ̂(window) is exactly the seam this test pins down.
    #[test]
    fn keeper_loop_auto_tunes_predictor_after_estimator_warms() {
        let mut env = StubBotEnv::new(StubView::new(vec![]));
        let mut keeper = KeeperLoop::new(
            KeeperLoopConfig::default(),
            Scheduler::new(SchedulerConfig::default()),
            RotateRiskPredictor::new(PredictorConfig {
                annual_vol: 0.10,
                ..PredictorConfig::default()
            }),
            RealizedVolatilityEstimator::new(vol_config_with(8, 2.5)),
        );

        // Below min_samples: predictor.annual_vol stays at the
        // hand-tuned 0.10.
        for slot in 0..4 {
            env.push_sample(PriceSample {
                price: 100 + slot * 7,
                slot: slot * 100,
            });
            let out = keeper.tick(&mut env).expect("tick");
            assert!(!out.metrics.vol_estimator_applied);
        }
        let pre = keeper.predictor().config().annual_vol;
        assert!((pre - 0.10).abs() < 1e-9);

        // Push enough samples to warm up the estimator.
        for slot in 4..16 {
            env.push_sample(PriceSample {
                price: 100 + slot * 7,
                slot: slot * 100,
            });
            keeper.tick(&mut env).expect("tick");
        }
        let post = keeper.predictor().config().annual_vol;
        assert_ne!(post, pre, "warm estimator must overwrite annual_vol");
        assert!(post > 0.0);
    }

    /// `auto_tune_vol = false` pins the predictor config — the
    /// estimator may warm up but the predictor never sees the
    /// updated σ. Required for the "lock the predictor while
    /// investigating a bug" operational mode.
    #[test]
    fn keeper_loop_auto_tune_off_pins_predictor() {
        let mut env = StubBotEnv::new(StubView::new(vec![]));
        let mut keeper = KeeperLoop::new(
            KeeperLoopConfig {
                run_predictor: false,
                auto_tune_vol: false,
            },
            Scheduler::new(SchedulerConfig::default()),
            RotateRiskPredictor::new(PredictorConfig {
                annual_vol: 0.42,
                ..PredictorConfig::default()
            }),
            RealizedVolatilityEstimator::new(vol_config_with(2, 2.5)),
        );
        for slot in 0..8 {
            env.push_sample(PriceSample {
                price: 100 + slot * 7,
                slot: slot * 100,
            });
            keeper.tick(&mut env).expect("tick");
        }
        assert_eq!(keeper.predictor().config().annual_vol, 0.42);
        assert!(keeper.vol_estimator().current_estimate().is_some(),
            "estimator did warm up but auto-tune is off, so predictor stayed pinned");
    }

    /// Mixed dispatch outcomes: an executor that fails alternating
    /// actions still lets the loop complete the tick, surfacing
    /// per-action results to the caller. This is the contract the
    /// production keeper bot leans on for retry policy.
    #[test]
    fn keeper_loop_records_partial_failure_metrics() {
        struct FlakyExecutor {
            n: usize,
        }
        impl ActionExecutor for FlakyExecutor {
            fn execute(&mut self, _action: KeeperAction) -> ActionDispatchResult {
                self.n += 1;
                if self.n.is_multiple_of(2) {
                    ActionDispatchResult::Failed {
                        reason: "simulated failure".to_string(),
                    }
                } else {
                    ActionDispatchResult::Submitted { signature: None }
                }
            }
        }

        struct FlakyEnv {
            view: StubView,
            executor: FlakyExecutor,
        }
        impl KeeperBotEnvironment for FlakyEnv {
            fn chain_view(&self) -> &dyn KeeperChainView {
                &self.view
            }
            fn fetch_price_sample(&mut self) -> Option<PriceSample> {
                None
            }
            fn executor(&mut self) -> &mut dyn ActionExecutor {
                &mut self.executor
            }
        }

        let view = StubView::new(vec![0]);
        let mut env = FlakyEnv {
            view,
            executor: FlakyExecutor { n: 0 },
        };

        let mut keeper = KeeperLoop::new(
            KeeperLoopConfig {
                run_predictor: false,
                auto_tune_vol: false,
            },
            Scheduler::new(SchedulerConfig::default()),
            RotateRiskPredictor::new(PredictorConfig::default()),
            RealizedVolatilityEstimator::new(vol_config_with(5, 2.5)),
        );
        for tick in 0..4 {
            keeper.scheduler_mut().record_init_hint(
                0,
                Direction::Long,
                tick as i64,
                InitRationale::Explicit,
            );
        }
        let outcome = keeper.tick(&mut env).expect("tick");
        assert_eq!(outcome.metrics.actions_planned, 4);
        assert_eq!(outcome.metrics.actions_submitted, 2);
        assert_eq!(outcome.metrics.actions_failed, 2);
        assert_eq!(outcome.metrics.actions_skipped, 0);
    }

    /// `KeeperLoopMetrics::merge` aggregates field-wise — the bot
    /// harness rolls per-tick metrics into a per-window dashboard
    /// snapshot using this helper.
    #[test]
    fn keeper_loop_metrics_merge_is_field_wise() {
        let a = KeeperLoopMetrics {
            price_samples_recorded: 1,
            init_hints_recorded: 2,
            actions_planned: 3,
            actions_submitted: 2,
            actions_failed: 1,
            actions_skipped: 0,
            vol_estimator_applied: false,
        };
        let b = KeeperLoopMetrics {
            price_samples_recorded: 1,
            init_hints_recorded: 1,
            actions_planned: 1,
            actions_submitted: 0,
            actions_failed: 0,
            actions_skipped: 1,
            vol_estimator_applied: true,
        };
        let merged = a.merge(b);
        assert_eq!(merged.price_samples_recorded, 2);
        assert_eq!(merged.init_hints_recorded, 3);
        assert_eq!(merged.actions_planned, 4);
        assert_eq!(merged.actions_submitted, 2);
        assert_eq!(merged.actions_failed, 1);
        assert_eq!(merged.actions_skipped, 1);
        assert!(merged.vol_estimator_applied);
    }
}
