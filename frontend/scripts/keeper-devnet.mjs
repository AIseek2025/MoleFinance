#!/usr/bin/env node
// ---------------------------------------------------------------------------
// MoleOption — devnet keeper / oracle bring-up.
//
// Devnet has no live oracle the on-chain program accepts (legacy Pyth v2 is
// frozen ~2y stale; the new Pyth pull layout is a different shape). So we run
// our own mock Pythnet-v2 oracle (program CLXte… deployed from
// programs/mock-oracle) and a keeper loop that, every tick, in ONE atomic tx:
//
//   1. set_price  → stamps our price account with a fresh price + current slot
//   2. sync_pool  → mole-option reads that price and writes SubPool.last_price
//
// Because both instructions land in the same tx, the oracle's pub_slot equals
// the slot sync_pool reads, so the staleness check is always age ≈ 0.
//
// `setup` is idempotent: it creates the price account, a SOL-PERP market wired
// to our oracle, its sub-pool, and both distribution ledgers (required by
// sync_pool). `run` loops set_price+sync_pool. Default `all` = setup then run.
//
// Usage:
//   export SOLANA_RPC_URL="https://devnet.helius-rpc.com/?api-key=..."
//   node frontend/scripts/keeper-devnet.mjs setup     # one-time on-chain init
//   node frontend/scripts/keeper-devnet.mjs run       # keeper loop (Ctrl-C to stop)
//   node frontend/scripts/keeper-devnet.mjs all       # setup + run
//
// Env overrides:
//   SOLANA_RPC_URL / SOLANA_WALLET / MOLE_PROGRAM_ID / MOCK_ORACLE_PROGRAM /
//   MARKET_SYMBOL (default SOL-PERP) / KEEPER_INTERVAL_MS (default 8000) /
//   COLLATERAL_MINT
// ---------------------------------------------------------------------------

import dns from "node:dns";
// Some sandboxes have no IPv6 egress; undici's Happy-Eyeballs can stall 10s on
// an AAAA record before falling back. Force IPv4-first resolution so RPC fetches
// connect immediately (curl works, plain `fetch` was timing out on :443).
dns.setDefaultResultOrder("ipv4first");

import { readFileSync, existsSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

import { sha256 } from "@noble/hashes/sha256";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_CLOCK_PUBKEY,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import * as borshNS from "@coral-xyz/borsh";
import BNimport from "bn.js";

const borsh = borshNS.default ?? borshNS;
const BN = BNimport.default ?? BNimport;

const HERE = dirname(fileURLToPath(import.meta.url));

const RPC_URL = process.env.SOLANA_RPC_URL || "https://api.devnet.solana.com";
const MOLE = new PublicKey(
  process.env.MOLE_PROGRAM_ID || "EWqyK5r4MyNaewo2s6nqLmZEVt4Xcx1yNT2dfFnTSfWp",
);
const MOCK_ORACLE = new PublicKey(
  process.env.MOCK_ORACLE_PROGRAM || "CLXteYm7SB9BgVmu4kC9GLhKjie9H5UmSs6czaNfcEQq",
);
const MARKET_SYMBOL = process.env.MARKET_SYMBOL || "SOL-PERP";
const INTERVAL_MS = Number(process.env.KEEPER_INTERVAL_MS || "8000");
const COLLATERAL_MINT = new PublicKey(
  process.env.COLLATERAL_MINT || "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU",
);

// Price account keypair persisted so setup + run + the market all agree on it.
const PRICE_KEYPAIR_PATH = join(HERE, "..", "..", "programs", "mock-oracle", "target", "deploy", "price-account.json");
const PRICE_ACCOUNT_SPACE = 512; // >= pyth-adapter MIN_HEADER_BYTES (240)
const PRICE_SCALE = 100_000_000; // 1e8 (matches pyth-adapter target expo -8)

const enc = new TextEncoder();
const sighash = (name) => Buffer.from(sha256(enc.encode(`global:${name}`)).slice(0, 8));
const acc = (pubkey, isSigner, isWritable) => ({ pubkey, isSigner, isWritable });

function ixData(name, layout, value) {
  const disc = sighash(name);
  if (!layout) return disc;
  const scratch = Buffer.alloc(4096);
  const len = layout.encode(value, scratch);
  return Buffer.concat([disc, scratch.subarray(0, len)]);
}

function symbolBytes16(symbol) {
  const raw = Buffer.from(symbol, "utf8");
  if (raw.length > 16) throw new Error(`symbol "${symbol}" > 16 bytes`);
  const out = Buffer.alloc(16);
  raw.copy(out);
  return out;
}
const u32le = (n) => { const b = Buffer.alloc(4); b.writeUInt32LE(n >>> 0, 0); return b; };

function loadWallet() {
  const path = process.env.SOLANA_WALLET || join(homedir(), ".config/solana/id.json");
  if (!existsSync(path)) throw new Error(`wallet keypair not found at ${path}`);
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))));
}

function loadOrCreatePriceKeypair() {
  if (existsSync(PRICE_KEYPAIR_PATH)) {
    return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(PRICE_KEYPAIR_PATH, "utf8"))));
  }
  const kp = Keypair.generate();
  writeFileSync(PRICE_KEYPAIR_PATH, JSON.stringify(Array.from(kp.secretKey)));
  console.log(`[keeper] generated price account keypair → ${PRICE_KEYPAIR_PATH}`);
  return kp;
}

// ── Borsh layouts (mirror programs/mole-option/src/instructions) ────────────
const INIT_MARKET_PARAMS = borsh.struct([
  borsh.array(borsh.u8(), 16, "symbol"),
  borsh.u32("leverageBps"),
  borsh.u64("minMargin"),
  borsh.u64("maxMarginPerPosition"),
  borsh.u128("maxTotalPrincipal"),
  borsh.u128("maxTotalNotional"),
  borsh.u16("openFeeBps"),
  borsh.i64("maxOracleAgeSeconds"),
  borsh.u64("maxOracleAgeSlots"),
  borsh.u16("maxConfidenceBps"),
  borsh.u32("maxPriceMoveBpsPerSync"),
  borsh.u64("priceTick"),
  borsh.u32("tickAggregationFactor"),
  borsh.u32("maxDormantBucketCountPerDirection"),
  borsh.u32("dilutionSafetyBps"),
  borsh.u64("maxIdleSlots"),
  borsh.u32("subPoolCount"),
  borsh.u8("dormantDistributeMode"),
  borsh.u32("maxPendingApplyPerTx"),
  borsh.u32("maxDistributionLedgerSize"),
]);
const SUB_POOL_ARGS = borsh.struct([borsh.u32("subPoolId")]);
const INIT_LEDGER_ARGS = borsh.struct([borsh.bool("directionIsLong")]);
const SYNC_ARGS = borsh.struct([
  borsh.u64("pNow"),
  borsh.u64("slot"),
  borsh.u64("expectedMin"),
  borsh.u64("expectedMax"),
  borsh.u32("longBucketCount"),
  borsh.u32("shortBucketCount"),
]);

function marketParams(symbol16) {
  return {
    symbol: Array.from(symbol16),
    leverageBps: 50_000,
    minMargin: new BN(1_000_000),
    maxMarginPerPosition: new BN(100_000_000_000),
    maxTotalPrincipal: new BN(1_000_000_000_000),
    maxTotalNotional: new BN(5_000_000_000_000),
    openFeeBps: 10,
    maxOracleAgeSeconds: new BN(120),
    maxOracleAgeSlots: new BN(500), // generous; keeper keeps age ~0 anyway
    maxConfidenceBps: 100,
    maxPriceMoveBpsPerSync: 5000, // 50% — comfortably covers the demo wiggle
    priceTick: new BN(100_000),
    tickAggregationFactor: 10,
    maxDormantBucketCountPerDirection: 64,
    dilutionSafetyBps: 100,
    maxIdleSlots: new BN(5000),
    subPoolCount: 1,
    dormantDistributeMode: 0,
    maxPendingApplyPerTx: 16,
    maxDistributionLedgerSize: 128,
  };
}

// mock-oracle set_price: raw program, data = price:i64 ++ conf:u64 (LE).
function setPriceData(price, conf) {
  const b = Buffer.alloc(16);
  b.writeBigInt64LE(BigInt(price), 0);
  b.writeBigUInt64LE(BigInt(conf), 8);
  return b;
}

// ── PDA derivation ───────────────────────────────────────────────────────────
function derivePdas(symbol16) {
  const [globalConfig] = PublicKey.findProgramAddressSync([Buffer.from("global_config")], MOLE);
  const [market] = PublicKey.findProgramAddressSync([Buffer.from("market"), symbol16], MOLE);
  const [vault] = PublicKey.findProgramAddressSync([Buffer.from("vault"), market.toBuffer()], MOLE);
  const [feeVault] = PublicKey.findProgramAddressSync([Buffer.from("fee_vault"), market.toBuffer()], MOLE);
  const [subPool] = PublicKey.findProgramAddressSync(
    [Buffer.from("sub_pool"), market.toBuffer(), u32le(0)], MOLE,
  );
  const [longLedger] = PublicKey.findProgramAddressSync(
    [Buffer.from("dist_ledger"), subPool.toBuffer(), Buffer.from([1])], MOLE,
  );
  const [shortLedger] = PublicKey.findProgramAddressSync(
    [Buffer.from("dist_ledger"), subPool.toBuffer(), Buffer.from([0])], MOLE,
  );
  return { globalConfig, market, vault, feeVault, subPool, longLedger, shortLedger };
}

async function sendTx(conn, wallet, ixs, signers = []) {
  const tx = new Transaction().add(...ixs);
  tx.feePayer = wallet.publicKey;
  const { blockhash, lastValidBlockHeight } = await conn.getLatestBlockhash("confirmed");
  tx.recentBlockhash = blockhash;
  tx.sign(wallet, ...signers);
  const sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: false, maxRetries: 5 });
  await conn.confirmTransaction({ signature: sig, blockhash, lastValidBlockHeight }, "confirmed");
  return sig;
}

// ── setup (idempotent) ────────────────────────────────────────────────────────
async function setup(conn, wallet, priceKp, pdas, symbol16) {
  // 1) price account owned by the mock oracle program
  const priceInfo = await conn.getAccountInfo(priceKp.publicKey);
  if (!priceInfo) {
    const rent = await conn.getMinimumBalanceForRentExemption(PRICE_ACCOUNT_SPACE);
    const createIx = SystemProgram.createAccount({
      fromPubkey: wallet.publicKey,
      newAccountPubkey: priceKp.publicKey,
      lamports: rent,
      space: PRICE_ACCOUNT_SPACE,
      programId: MOCK_ORACLE,
    });
    const seedIx = new TransactionInstruction({
      programId: MOCK_ORACLE,
      keys: [acc(priceKp.publicKey, false, true)],
      data: setPriceData(140 * PRICE_SCALE, 3_500_000),
    });
    const sig = await sendTx(conn, wallet, [createIx, seedIx], [priceKp]);
    console.log(`[keeper] price account created + seeded: ${priceKp.publicKey.toBase58()}  ${sig}`);
  } else {
    console.log(`[keeper] price account exists: ${priceKp.publicKey.toBase58()}`);
  }

  // 2) market wired to our oracle
  if (!(await conn.getAccountInfo(pdas.market))) {
    const ix = new TransactionInstruction({
      programId: MOLE,
      keys: [
        acc(pdas.globalConfig, false, false),
        acc(pdas.market, false, true),
        acc(COLLATERAL_MINT, false, false),
        acc(pdas.vault, false, false),
        acc(pdas.feeVault, false, false),
        acc(priceKp.publicKey, false, false), // oracle_price_feed
        acc(MOCK_ORACLE, false, false), // oracle_program
        acc(wallet.publicKey, true, true), // admin
        acc(wallet.publicKey, true, true), // payer
        acc(SystemProgram.programId, false, false),
      ],
      data: ixData("initialize_market", INIT_MARKET_PARAMS, marketParams(symbol16)),
    });
    console.log(`[keeper] initialize_market ${MARKET_SYMBOL}: ${await sendTx(conn, wallet, [ix])}`);
  } else {
    console.log(`[keeper] market exists: ${pdas.market.toBase58()}`);
  }

  // 3) sub-pool 0
  if (!(await conn.getAccountInfo(pdas.subPool))) {
    const ix = new TransactionInstruction({
      programId: MOLE,
      keys: [
        acc(pdas.market, false, false),
        acc(pdas.subPool, false, true),
        acc(pdas.globalConfig, false, true),
        acc(wallet.publicKey, true, true),
        acc(SystemProgram.programId, false, false),
      ],
      data: ixData("initialize_sub_pool", SUB_POOL_ARGS, { subPoolId: 0 }),
    });
    console.log(`[keeper] initialize_sub_pool 0: ${await sendTx(conn, wallet, [ix])}`);
  } else {
    console.log(`[keeper] sub_pool exists: ${pdas.subPool.toBase58()}`);
  }

  // 4) distribution ledgers (sync_pool requires both)
  for (const [dir, ledger, label] of [[true, pdas.longLedger, "long"], [false, pdas.shortLedger, "short"]]) {
    if (!(await conn.getAccountInfo(ledger))) {
      const ix = new TransactionInstruction({
        programId: MOLE,
        keys: [
          acc(pdas.market, false, false),
          acc(pdas.subPool, false, false),
          acc(ledger, false, true),
          acc(wallet.publicKey, true, true),
          acc(SystemProgram.programId, false, false),
        ],
        data: ixData("initialize_distribution_ledger", INIT_LEDGER_ARGS, { directionIsLong: dir }),
      });
      console.log(`[keeper] initialize_distribution_ledger (${label}): ${await sendTx(conn, wallet, [ix])}`);
    } else {
      console.log(`[keeper] ${label} ledger exists: ${ledger.toBase58()}`);
    }
  }
}

function buildSetPriceIx(priceKp, price, conf) {
  return new TransactionInstruction({
    programId: MOCK_ORACLE,
    keys: [acc(priceKp.publicKey, false, true)],
    data: setPriceData(price, conf),
  });
}

function buildSyncIx(pdas, priceKp) {
  return new TransactionInstruction({
    programId: MOLE,
    keys: [
      acc(pdas.subPool, false, true),
      acc(pdas.market, false, false),
      acc(pdas.longLedger, false, true),
      acc(pdas.shortLedger, false, true),
      acc(priceKp.publicKey, false, false), // oracle_price_feed (address-pinned)
      acc(SYSVAR_CLOCK_PUBKEY, false, false),
    ],
    // p_now/slot overwritten on-chain by the validated oracle price;
    // wide expected band so the trusted price always lands inside it.
    data: ixData("sync_pool", SYNC_ARGS, {
      pNow: new BN(0),
      slot: new BN(0),
      expectedMin: new BN(1),
      expectedMax: new BN("100000000000000"),
      longBucketCount: 0,
      shortBucketCount: 0,
    }),
  });
}

async function readLastPrice(conn, subPool) {
  const ai = await conn.getAccountInfo(subPool);
  if (!ai) return null;
  // SubPool layout: disc(8) market(32) sub_pool_id(4) long_pool_equity(16)
  // short_pool_equity(16) long_active_shares(16) short_active_shares(16)
  // long_recovery_shares(16) short_recovery_shares(16) long_active_notional(16)
  // short_active_notional(16) long_active_generation(8) short_active_generation(8)
  // last_price(8) ...
  const off = 8 + 32 + 4 + 16 * 8 + 8 + 8;
  return ai.data.readBigUInt64LE(off);
}

async function run(conn, wallet, priceKp, pdas) {
  console.log(`[keeper] loop every ${INTERVAL_MS}ms — set_price + sync_pool (Ctrl-C to stop)`);
  let tick = 0;
  const base = 140 * PRICE_SCALE;
  let stop = false;
  process.on("SIGINT", () => { stop = true; console.log("\n[keeper] stopping…"); });
  while (!stop) {
    // Demo wiggle: ±$2 sine + small jitter around $140.
    const wiggle = Math.round(Math.sin(tick / 3) * 2 * PRICE_SCALE + (Math.random() - 0.5) * 0.4 * PRICE_SCALE);
    const price = base + wiggle;
    const conf = 3_500_000;
    try {
      const sig = await sendTx(conn, wallet, [buildSetPriceIx(priceKp, price, conf), buildSyncIx(pdas, priceKp)]);
      const lp = await readLastPrice(conn, pdas.subPool);
      const usd = lp === null ? "?" : (Number(lp) / PRICE_SCALE).toFixed(4);
      console.log(`[keeper] tick ${tick}: pushed $${(price / PRICE_SCALE).toFixed(4)} → SubPool.last_price=$${usd}  ${sig.slice(0, 8)}…`);
    } catch (e) {
      console.warn(`[keeper] tick ${tick} failed: ${e.message}`);
    }
    tick += 1;
    await new Promise((r) => setTimeout(r, INTERVAL_MS));
  }
}

async function main() {
  const mode = process.argv[2] || "all";
  const wallet = loadWallet();
  const conn = new Connection(RPC_URL, "confirmed");
  const priceKp = loadOrCreatePriceKeypair();
  const symbol16 = symbolBytes16(MARKET_SYMBOL);
  const pdas = derivePdas(symbol16);

  console.log("[keeper] rpc:        " + RPC_URL.replace(/api-key=[^&]+/, "api-key=***"));
  console.log("[keeper] mole:       " + MOLE.toBase58());
  console.log("[keeper] mockOracle: " + MOCK_ORACLE.toBase58());
  console.log("[keeper] wallet:     " + wallet.publicKey.toBase58());
  console.log("[keeper] symbol:     " + MARKET_SYMBOL);
  console.log("[keeper] market:     " + pdas.market.toBase58());
  console.log("[keeper] subPool:    " + pdas.subPool.toBase58());
  console.log("[keeper] priceAcct:  " + priceKp.publicKey.toBase58());

  if (mode === "setup" || mode === "all") await setup(conn, wallet, priceKp, pdas, symbol16);
  if (mode === "run" || mode === "all") await run(conn, wallet, priceKp, pdas);

  if (mode === "setup") {
    console.log("\n[keeper] setup done. Point the frontend at this market:");
    console.log(`  VITE_MARKET_PDA=${pdas.market.toBase58()}`);
    console.log("Then run the loop:  node frontend/scripts/keeper-devnet.mjs run");
  }
}

main().catch((e) => { console.error("[keeper] FAILED:", e?.message || e); process.exit(1); });
