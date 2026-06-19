# `ops-toolkit/ts/` — keeper-leader-lock CLI scripts

Wave 17 deliverable. Companion to the Rust health-check binary in
`crates/ops-toolkit/`. Where the Rust crate is **read-only** (it
inspects an already-fetched `HealthContext`), these TypeScript
scripts are **write-side**: they build, sign, and submit the four
keeper-leader instructions referenced in
`Docs/Planning/24-operator-runbook.md` §6.5.

## Why TypeScript?

- The Solana web3.js + Anchor IDL story is significantly less
  painful in TS than in Rust for one-shot ops scripts.
- Keepers run their own daemons in Rust (`crates/keeper-bot/`); ops
  needs a *different* tool — interactive, one-instruction-per-run,
  works against any keypair file an operator has on disk.
- Wave-15 publishes the same wasm encoders to the frontend; the
  scripts re-use the very same `keeper_decoder::ix` byte layout via
  hand-encoded discriminators (see `lib.ts` for the layout).

## Available scripts

| Script | Runbook reference | One-line |
|---|---|---|
| `keeper-leader-init.ts` | KL-01 | Initialize the lock PDA (one-shot per market) |
| `keeper-leader-show.ts` | KL-01..05 | Decode + print the on-chain lock state |
| `keeper-leader-acquire.ts` | KL-03 | Force-acquire (only when current holder is stale) |
| `keeper-leader-heartbeat.ts` | KL-02, KL-08 | Manually publish a heartbeat (debug / probe) |
| `keeper-leader-release.ts` | KL-02 | Manually release the lock (planned handoff) |

## Running

```bash
# 1. Install deps once.
cd ops-toolkit/ts && npm install

# 2. Run any of the scripts via ts-node:
npx ts-node keeper-leader-show.ts --market "$MARKET_PDA"
```

All scripts read `MOLE_RPC_URL`, `MOLE_PROGRAM_ID` (or `--rpc` /
`--program` flags) and require a JSON keypair file passed via
`--payer` / `--keeper`. None of the scripts touch a wallet without
the operator's explicit `--confirm` flag — see each script's
`--help` for the per-script argument list.

## Determinism

The scripts re-derive the PDA seeds locally (no `getProgramAccounts`
fallback) so they're safe to run against an outage-mode RPC: an
unreachable RPC fails fast with a network error rather than
silently submitting against the wrong PDA.
