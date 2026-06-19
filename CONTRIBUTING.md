# Contributing to MoleOption

> This file is the **operational** companion to `SECURITY.md`. Where
> `SECURITY.md` says *what* invariants the protocol claims, this file
> says *how* a contributor or external auditor verifies those claims
> on their own machine in 5 minutes.

If you're an external **audit firm**, start at section §3 below. If
you're an external **contributor**, start at §1.

---

## 1 First-time setup

```bash
git clone https://github.com/<org>/MoleOption
cd MoleOption

# Rust toolchain (workspace pins on stable)
rustup toolchain install stable
rustup component add clippy rustfmt

# Frontend toolchain
cd frontend && npm ci && cd ..
```

You should now be able to:

```bash
cargo build --workspace
cargo test --workspace               # 262 tests, ~25 s on a fresh machine
cargo clippy --workspace --all-targets -- -D warnings

cd frontend
npm run typecheck
npm run build
cd ..
```

If any of those fail on a clean checkout, please open an issue —
that's a CI gap on our side.

---

## 2 The four CI gates

Before submitting a PR, run all four locally. These are the same gates
`.github/workflows/ci.yml` runs in CI; if you pass them locally, your
PR's CI will pass.

```bash
# Gate 1 — Rust tests, default features.
cargo test --workspace --all-targets

# Gate 2 — Rust tests, production feature on.
cargo test -p keeper-rpc --features solana-rpc

# Gate 3 — Lint, default + feature.
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p keeper-rpc --features solana-rpc --all-targets -- -D warnings

# Gate 4 — Frontend.
( cd frontend && npm run typecheck && npm run build )
```

In addition we run **three governance scripts** that block the PR
without changing any code:

```bash
# G-1 — Every test reference in SECURITY.md resolves to a live `fn`.
bash scripts/verify-security-references.sh

# G-2 — `cargo test --workspace` total matches the count claimed in
#        Docs/Planning/20-…md.
bash scripts/verify-test-counts.sh

# G-3 — Every `pub <field>:` on the Onchain* Borsh mirrors has a row
#        in Docs/SCHEMA-MAPPING.md (mapped to a TS surface OR
#        explicitly omitted with a rationale).
bash scripts/verify-schema-parity.sh
```

If you add a Rust schema field, the third script will fail until you
add a row to `Docs/SCHEMA-MAPPING.md`. If you rename a test fn that
`SECURITY.md` cites, the first script will fail until you update both.
This is the deliberate cost of keeping audit-facing docs honest.

---

## 3 For audit firms

Two-day onboarding sequence:

### Day 1 — Documentation read

In order:

1. `README.md` — project elevator pitch (≈ 5 min).
2. `Docs/Planning/00-Whitepaper.md` — protocol design (≈ 90 min).
3. `Docs/Planning/20-攻坚开发进度与里程碑.md` — wave-by-wave delivery
   ledger; the **most recent wave's §X.5** verification block tells
   you exactly what state the codebase is in (≈ 30 min).
4. `SECURITY.md` — invariant catalogue, threat model, trust
   assumptions (≈ 60 min).
5. `Docs/SCHEMA-MAPPING.md` — Rust ↔ TypeScript schema-layer mapping
   (≈ 20 min).
6. `Docs/Planning/24-operator-runbook.md` — incident playbooks +
   alert thresholds (≈ 60 min).
7. `CHANGELOG.md` — wave-by-wave externally-readable history
   (≈ 15 min).

### Day 2 — Code read

In order:

1. `programs/mole-option/src/state.rs` — on-chain account layout
   (~700 lines).
2. `programs/mole-option/src/instructions/*.rs` — every Anchor
   handler (~2 000 lines total).
3. `crates/clearing-core/src/{lib,engine,market,dormant}.rs` —
   the math engine the on-chain handlers call (~3 500 lines).
4. `crates/clearing-core/src/invariants.rs` — runtime invariants
   asserted on every state transition.
5. `crates/keeper/src/lib.rs` — keeper bot state machine (~2 200
   lines).
6. `crates/keeper-rpc/src/{accounts,snapshot,solana}.rs` —
   on-chain ↔ off-chain bridge.

### Local repro — 5 commands

```bash
# 1. Compile + run the full test suite.
cargo test --workspace --all-targets   # 262 / 262 pass

# 2. Production feature.
cargo test -p keeper-rpc --features solana-rpc   # 32 / 32 pass

# 3. Run the keeper bot for 1 tick + auto-shutdown.
cargo run -p keeper-bot --bin keeper-bot --quiet -- serve 127.0.0.1:0 1

# 4. Run the ops health prober — happy path + broken-state path.
cargo run -p ops-toolkit -- demo human          # exit 0
cargo run -p ops-toolkit -- demo-broken human   # exit 4

# 5. Frontend MVP.
cd frontend && npm ci && npm run dev
# → open http://localhost:5173
```

### Audit focus areas

Per `SECURITY.md §1.1`, the seven adversaries map to focus areas:

| Adversary | Code area to audit hardest |
|-----------|----------------------------|
| **A-1** Trader | `crates/clearing-core/src/{engine,position}.rs` — pool-equity floors, dilution safety, force-close gates. |
| **A-2** Coalition | `crates/protocol-harness/tests/random_workload.rs` and `adversarial.rs` — does multi-trader random walk break the conservation invariant? |
| **A-3** MEV | `programs/mole-option/src/instructions/{open,close,sync,pre_sync}.rs` — atomicity, cross-tx state isolation. |
| **A-4** Malicious keeper | `crates/keeper/src/lib.rs` (the `Scheduler::plan` filter), `crates/keeper-rpc/src/tx.rs` (Anchor discriminator pinning). |
| **A-5** Compromised governance | `programs/mole-option/src/instructions/admin.rs` + `migration.rs`; `crates/clearing-core/tests/safety_gates.rs` (every `*_rejects_stale_*_schema` test). |
| **A-6** Pyth outage / manipulation | `crates/clearing-core/tests/safety_gates.rs::envelope` invariants; `programs/mole-option/src/instructions/sync.rs` oracle-age gate. |
| **A-7** Frontend / RPC adversary | `frontend/src/wallet/windowWalletAdapter.ts` (signs only what the user sees) and `crates/keeper-rpc/src/snapshot.rs` (schema-version mismatch refuses to act). |

---

## 4 Reporting issues

- **Security**: see `SECURITY.md §4` (PGP / Signal / GitHub Advisory).
  Don't open public GitHub issues for security bugs.
- **Bugs**: open a regular GitHub issue with steps to reproduce.
- **Feature requests**: open a GitHub issue tagged `proposal`. We'll
  triage against the wave plan in
  `Docs/Planning/20-攻坚开发进度与里程碑.md §future`.

---

## 5 PR conventions

- One PR per wave deliverable. Don't bundle frontend changes with
  governance changes.
- Reference the wave / phase number in the title:
  `[wave 13] CI infrastructure + SECURITY.md repair`.
- All four CI gates and three governance scripts must pass.
- Update `CHANGELOG.md` with an entry under the current wave's
  section, even for tiny changes.
- Update `Docs/Planning/20-攻坚开发进度与里程碑.md` for any change
  that affects the test count or adds a delivery line.
- Tests that prove a new `SECURITY.md` invariant must be cited in the
  invariant table — `scripts/verify-security-references.sh` enforces
  this.

---

## 6 Trust boundaries — what we DO and DON'T promise

- **DO** preserve every CORE-* / ONCH-* / GOVN-* / KEEP-* invariant
  catalogued in `SECURITY.md`. A change that knowingly breaks one is
  a non-starter.
- **DO** maintain `cargo test --workspace` at green for every commit
  on `main`.
- **DO** maintain `clippy -D warnings` clean for every commit on
  `main`.
- **DON'T** promise stability of internal Rust APIs across crates
  until we hit `1.0`. The `keeper-rpc` and `keeper` traits MAY change
  shape; the `clearing-core` engine API and the on-chain Anchor
  account layouts (governed by `schema_version`) won't change without
  a wave-9-style migration path.
- **DON'T** promise frontend type stability — the frontend is
  pre-MVP and will refactor freely until a wave dedicated to "stable
  external integration API" lands.

---

Thank you. Honest engineering of a safety-critical protocol depends
on independent eyes; we're glad you're here.
