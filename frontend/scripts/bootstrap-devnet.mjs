#!/usr/bin/env node
// ---------------------------------------------------------------------------
// MoleOption — devnet protocol bootstrap.
//
// Sends the three init instructions against the already-deployed program so
// the frontend can show *real* on-chain data:
//
//   1. initialize_global_config   → GlobalConfig PDA  [b"global_config"]
//   2. initialize_market          → Market PDA        [b"market", symbol(16)]
//   3. initialize_sub_pool * N     → SubPool PDA       [b"sub_pool", market, id]
//
// The oracle is wired to the existing **legacy Pythnet-v2 SOL/USD** account on
// devnet (the on-chain `pyth-adapter` validates the v2 magic 0xa1b2c3d4, NOT
// the new Pyth pull `PriceUpdateV2` layout). At init time the program only
// *stores* the oracle pubkeys — they are validated later in `sync_pool`. So
// even if the legacy devnet feed has gone stale, the bootstrap still succeeds
// and the protocol surfaces in the UI; you only need a live feed (or a mock
// oracle) once you start syncing/trading.
//
// No Anchor CLI / IDL required: instruction data = 8-byte sighash
// (sha256("global:<name>")[..8]) + Borsh(params), encoded with @coral-xyz/borsh.
//
// Usage (run from the repo root or anywhere; resolves deps from frontend/):
//
//   # dry run — derive every PDA + encode every ix, but send nothing
//   node frontend/scripts/bootstrap-devnet.mjs --dry-run
//
//   # real run against devnet (uses ~/.config/solana/id.json by default)
//   export SOLANA_RPC_URL="https://devnet.helius-rpc.com/?api-key=..."
//   node frontend/scripts/bootstrap-devnet.mjs
//
// Env overrides (all optional):
//   SOLANA_RPC_URL      RPC endpoint            (default: api.devnet.solana.com)
//   SOLANA_WALLET       payer/admin keypair     (default: ~/.config/solana/id.json)
//   MOLE_PROGRAM_ID     deployed program id     (default: hard-coded below)
//   MARKET_SYMBOL       up to 16 ASCII bytes    (default: SOL-USD)
//   PYTH_PROGRAM        legacy pyth program id  (default: gSbe...92s, devnet)
//   PYTH_SOL_USD_FEED   legacy v2 price account (default: J83w...kix,  devnet)
//   COLLATERAL_MINT     collateral mint pubkey  (default: devnet USDC)
//   SUB_POOL_COUNT      sub-pools to create     (default: 1)
// ---------------------------------------------------------------------------

import { readFileSync, existsSync, appendFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

import { sha256 } from "@noble/hashes/sha256";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import * as borshNS from "@coral-xyz/borsh";
import BNimport from "bn.js";

const borsh = borshNS.default ?? borshNS;
const BN = BNimport.default ?? BNimport;

// ── config ────────────────────────────────────────────────────────────────
const DRY_RUN = process.argv.includes("--dry-run");

const RPC_URL = process.env.SOLANA_RPC_URL || "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(
  process.env.MOLE_PROGRAM_ID || "EWqyK5r4MyNaewo2s6nqLmZEVt4Xcx1yNT2dfFnTSfWp",
);
const MARKET_SYMBOL = process.env.MARKET_SYMBOL || "SOL-USD";
const SUB_POOL_COUNT = Number(process.env.SUB_POOL_COUNT || "1");

// Legacy Pythnet-v2 SOL/USD on devnet (owned by the legacy pyth program).
const PYTH_PROGRAM = new PublicKey(
  process.env.PYTH_PROGRAM || "gSbePebfvPy7tRqimPoVecS2UsBvYv46ynrzWocc92s",
);
const PYTH_SOL_USD_FEED = new PublicKey(
  process.env.PYTH_SOL_USD_FEED || "J83w4HKfqxwcq3BEMMkPFSppX3gqekLyLJBexebFVkix",
);
// Circle devnet USDC. Only *stored* in the Market at init; not validated here.
const COLLATERAL_MINT = new PublicKey(
  process.env.COLLATERAL_MINT || "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU",
);

// ── helpers ─────────────────────────────────────────────────────────────────
const enc = new TextEncoder();

/** 8-byte Anchor instruction sighash: sha256("global:<name>")[..8]. */
function sighash(name) {
  return Buffer.from(sha256(enc.encode(`global:${name}`)).slice(0, 8));
}

/** disc(8) ++ Borsh(params) → instruction data Buffer. */
function ixData(name, layout, value) {
  const disc = sighash(name);
  if (!layout) return disc;
  const scratch = Buffer.alloc(4096);
  const len = layout.encode(value, scratch);
  return Buffer.concat([disc, scratch.subarray(0, len)]);
}

function acc(pubkey, isSigner, isWritable) {
  return { pubkey, isSigner, isWritable };
}

/** "SOL-USD" → 16-byte buffer (ASCII, zero-padded). Used for PDA seed + param. */
function symbolBytes16(symbol) {
  const raw = Buffer.from(symbol, "utf8");
  if (raw.length > 16) {
    throw new Error(`MARKET_SYMBOL "${symbol}" exceeds 16 bytes`);
  }
  const out = Buffer.alloc(16);
  raw.copy(out);
  return out;
}

function u32le(n) {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(n >>> 0, 0);
  return b;
}

function loadWallet() {
  const path = process.env.SOLANA_WALLET || join(homedir(), ".config/solana/id.json");
  if (!existsSync(path)) {
    if (DRY_RUN) {
      console.log(`[bootstrap] (dry-run) wallet ${path} not found — using ephemeral keypair`);
      return Keypair.generate();
    }
    throw new Error(`wallet keypair not found at ${path} (set SOLANA_WALLET)`);
  }
  const secret = Uint8Array.from(JSON.parse(readFileSync(path, "utf8")));
  return Keypair.fromSecretKey(secret);
}

// ── Borsh param layouts (field order MUST mirror programs/.../init.rs) ───────
const GLOBAL_CONFIG_PARAMS = borsh.struct([
  borsh.publicKey("adminAuthority"),
  borsh.publicKey("emergencyAuthority"),
  borsh.publicKey("protocolTreasury"),
  borsh.publicKey("upgradeAuthority"),
]);

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

// Demo SOL-USD market params (USDC has 6 decimals; price scale = 1e8).
function marketParams(symbol16) {
  return {
    symbol: Array.from(symbol16),
    leverageBps: 50_000, // 5x
    minMargin: new BN(1_000_000), // 1 USDC
    maxMarginPerPosition: new BN(100_000_000_000), // 100k USDC
    maxTotalPrincipal: new BN(1_000_000_000_000), // 1M USDC
    maxTotalNotional: new BN(5_000_000_000_000), // 5M USDC
    openFeeBps: 10,
    maxOracleAgeSeconds: new BN(60),
    maxOracleAgeSlots: new BN(150),
    maxConfidenceBps: 100,
    maxPriceMoveBpsPerSync: 1000,
    priceTick: new BN(100_000), // 0.001 USD @ 1e8
    tickAggregationFactor: 10,
    maxDormantBucketCountPerDirection: 64,
    dilutionSafetyBps: 100,
    maxIdleSlots: new BN(5000),
    subPoolCount: SUB_POOL_COUNT,
    dormantDistributeMode: 0, // Eager
    maxPendingApplyPerTx: 16,
    maxDistributionLedgerSize: 128,
  };
}

// ── main ─────────────────────────────────────────────────────────────────────
async function main() {
  const wallet = loadWallet();
  const conn = new Connection(RPC_URL, "confirmed");
  const symbol16 = symbolBytes16(MARKET_SYMBOL);

  const [globalConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("global_config")],
    PROGRAM_ID,
  );
  const [market] = PublicKey.findProgramAddressSync(
    [Buffer.from("market"), symbol16],
    PROGRAM_ID,
  );
  // vault / fee_vault are AccountInfo only stored at init (never created here),
  // so deterministic off-curve PDAs are a fine placeholder for the display
  // bootstrap. Replace with real ATAs before enabling deposits/trading.
  const [vault] = PublicKey.findProgramAddressSync(
    [Buffer.from("vault"), market.toBuffer()],
    PROGRAM_ID,
  );
  const [feeVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("fee_vault"), market.toBuffer()],
    PROGRAM_ID,
  );

  console.log("[bootstrap] " + (DRY_RUN ? "DRY RUN (no transactions sent)" : "LIVE"));
  console.log("[bootstrap] rpc:        " + RPC_URL.replace(/api-key=[^&]+/, "api-key=***"));
  console.log("[bootstrap] program:    " + PROGRAM_ID.toBase58());
  console.log("[bootstrap] wallet:     " + wallet.publicKey.toBase58());
  console.log("[bootstrap] symbol:     " + MARKET_SYMBOL);
  console.log("[bootstrap] globalCfg:  " + globalConfig.toBase58());
  console.log("[bootstrap] market:     " + market.toBase58());
  console.log("[bootstrap] vault:      " + vault.toBase58() + " (placeholder)");
  console.log("[bootstrap] feeVault:   " + feeVault.toBase58() + " (placeholder)");
  console.log("[bootstrap] oracle:     " + PYTH_SOL_USD_FEED.toBase58());
  console.log("[bootstrap] oracleProg: " + PYTH_PROGRAM.toBase58());
  console.log("[bootstrap] collateral: " + COLLATERAL_MINT.toBase58());

  // 1) initialize_global_config ------------------------------------------------
  const gcIx = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      acc(globalConfig, false, true),
      acc(wallet.publicKey, true, true), // payer
      acc(SystemProgram.programId, false, false),
    ],
    data: ixData("initialize_global_config", GLOBAL_CONFIG_PARAMS, {
      adminAuthority: wallet.publicKey,
      emergencyAuthority: wallet.publicKey,
      protocolTreasury: wallet.publicKey,
      upgradeAuthority: wallet.publicKey,
    }),
  });

  // 2) initialize_market -------------------------------------------------------
  const mIx = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      acc(globalConfig, false, false),
      acc(market, false, true),
      acc(COLLATERAL_MINT, false, false),
      acc(vault, false, false),
      acc(feeVault, false, false),
      acc(PYTH_SOL_USD_FEED, false, false),
      acc(PYTH_PROGRAM, false, false),
      acc(wallet.publicKey, true, true), // admin (== global_config.admin_authority)
      acc(wallet.publicKey, true, true), // payer
      acc(SystemProgram.programId, false, false),
    ],
    data: ixData("initialize_market", INIT_MARKET_PARAMS, marketParams(symbol16)),
  });

  // 3) initialize_sub_pool * N -------------------------------------------------
  const subPoolPlan = [];
  for (let id = 0; id < SUB_POOL_COUNT; id += 1) {
    const [subPool] = PublicKey.findProgramAddressSync(
      [Buffer.from("sub_pool"), market.toBuffer(), u32le(id)],
      PROGRAM_ID,
    );
    const spIx = new TransactionInstruction({
      programId: PROGRAM_ID,
      keys: [
        acc(market, false, false),
        acc(subPool, false, true),
        acc(globalConfig, false, true),
        acc(wallet.publicKey, true, true), // payer
        acc(SystemProgram.programId, false, false),
      ],
      data: ixData("initialize_sub_pool", SUB_POOL_ARGS, { subPoolId: id }),
    });
    subPoolPlan.push({ id, subPool, ix: spIx });
    console.log(`[bootstrap] subPool[${id}]: ${subPool.toBase58()}`);
  }

  const steps = [
    { name: "initialize_global_config", pda: globalConfig, ix: gcIx },
    { name: "initialize_market", pda: market, ix: mIx },
    ...subPoolPlan.map((s) => ({
      name: `initialize_sub_pool(${s.id})`,
      pda: s.subPool,
      ix: s.ix,
    })),
  ];

  if (DRY_RUN) {
    console.log("\n[bootstrap] encoded instruction data lengths:");
    for (const s of steps) {
      console.log(`  ${s.name}: ${s.ix.data.length} bytes, ${s.ix.keys.length} accounts`);
    }
    console.log("\n[bootstrap] dry run OK — nothing was sent.");
    printFrontendConfig(market);
    return;
  }

  for (const s of steps) {
    const existing = await conn.getAccountInfo(s.pda);
    if (existing) {
      console.log(`[bootstrap] ${s.name}: ${s.pda.toBase58()} already exists — skip`);
      continue;
    }
    const tx = new Transaction().add(s.ix);
    tx.feePayer = wallet.publicKey;
    const { blockhash, lastValidBlockHeight } = await conn.getLatestBlockhash("confirmed");
    tx.recentBlockhash = blockhash;
    tx.sign(wallet);
    console.log(`[bootstrap] ${s.name}: sending…`);
    const sig = await conn.sendRawTransaction(tx.serialize(), {
      skipPreflight: false,
      maxRetries: 5,
    });
    await conn.confirmTransaction({ signature: sig, blockhash, lastValidBlockHeight }, "confirmed");
    console.log(`[bootstrap] ${s.name}: OK  ${sig}`);
  }

  console.log("\n[bootstrap] protocol bootstrapped ✔");
  console.log(
    "[bootstrap] explorer: " +
      `https://explorer.solana.com/address/${market.toBase58()}?cluster=devnet`,
  );
  printFrontendConfig(market);
}

function printFrontendConfig(market) {
  const here = dirname(fileURLToPath(import.meta.url));
  const envPath = join(here, "..", ".env.local");
  const line = `VITE_MARKET_PDA=${market.toBase58()}`;
  const marketsJson = JSON.stringify([
    {
      symbol: MARKET_SYMBOL,
      programId: PROGRAM_ID.toBase58(),
      marketPda: market.toBase58(),
    },
  ]);

  console.log("\n[bootstrap] frontend config — add to frontend/.env.local:");
  console.log(`  VITE_RPC_URL=${RPC_URL}`);
  console.log(`  VITE_MOLE_PROGRAM_ID=${PROGRAM_ID.toBase58()}`);
  console.log(`  ${line}`);
  console.log(`  # or multi-market: VITE_MARKETS='${marketsJson}'`);

  if (!DRY_RUN && existsSync(envPath)) {
    try {
      const cur = readFileSync(envPath, "utf8");
      if (!cur.includes("VITE_MARKET_PDA=")) {
        appendFileSync(envPath, (cur.endsWith("\n") ? "" : "\n") + line + "\n");
        console.log(`[bootstrap] appended VITE_MARKET_PDA to ${envPath}`);
      }
    } catch (e) {
      console.warn(`[bootstrap] could not update ${envPath}: ${e.message}`);
    }
  }
}

main().catch((e) => {
  console.error("[bootstrap] FAILED:", e?.message || e);
  process.exit(1);
});
