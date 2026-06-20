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
import net from "node:net";
// Some hosts have broken/half-open IPv6 egress; undici's Happy-Eyeballs
// (autoSelectFamily, default ON in Node 20+) races A+AAAA and can stall or
// fail the connect, surfacing as `TypeError: fetch failed`
// (UND_ERR_CONNECT_TIMEOUT) in web3.js even though `curl` / the rust solana
// CLI connect fine. Force IPv4-first DNS *and* disable family autoselection
// so connects go straight to the IPv4 address.
dns.setDefaultResultOrder("ipv4first");
if (typeof net.setDefaultAutoSelectFamily === "function") {
  net.setDefaultAutoSelectFamily(false);
}

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
  SYSVAR_RENT_PUBKEY,
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
const TOKEN_PROGRAM = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

// Price account keypairs persisted so setup + run + the market all agree on
// them. One price account is shared by ALL leverage tiers of the same base
// (they track the same underlying), keyed by base ticker.
const PRICE_KEYPAIR_DIR = join(HERE, "..", "..", "programs", "mock-oracle", "target", "deploy");
const priceKeypairPathFor = (base) => join(PRICE_KEYPAIR_DIR, `price-${base}.json`);
const PRICE_ACCOUNT_SPACE = 512; // >= pyth-adapter MIN_HEADER_BYTES (240)
const PRICE_SCALE = 100_000_000; // 1e8 (matches pyth-adapter target expo -8)

// ── Market catalog (single source of truth, shared with the frontend) ───────
const CATALOG_PATH = join(HERE, "..", "src", "markets", "catalog.json");

function loadCatalog() {
  return JSON.parse(readFileSync(CATALOG_PATH, "utf8"));
}

/**
 * Expand the catalog into the concrete (base, leverage) markets to provision.
 * Each leverage tier is an independent on-chain Market keyed `${base}-${lev}X`.
 *
 * Env knobs (keep devnet load sane — the frontend shows the FULL catalog with
 * synthetic prices regardless, so the keeper only needs to back the markets
 * you want trade-real):
 *   MARKET_BASES  — comma list of bases, or "ALL"   (default: a 6-symbol set)
 *   MARKET_TIERS  — comma list of tiers, or "ALL"   (default: ALL, per cap)
 */
function buildMarketList(catalog) {
  const tiersAll = catalog.leverageTiers;
  const classes = catalog.assetClasses;
  const basesEnv = (process.env.MARKET_BASES || "BTC,ETH,SOL,SP500,GOLD,EURUSD").trim();
  const tiersEnv = (process.env.MARKET_TIERS || "ALL").trim();
  const baseFilter =
    basesEnv.toUpperCase() === "ALL" ? null : new Set(basesEnv.split(",").map((s) => s.trim()));
  const tierFilter =
    tiersEnv.toUpperCase() === "ALL" ? null : new Set(tiersEnv.split(",").map((s) => Number(s.trim())));
  const out = [];
  for (const sym of catalog.symbols) {
    if (baseFilter && !baseFilter.has(sym.base)) continue;
    const cap = classes[sym.class].maxLeverage;
    for (const lev of tiersAll) {
      if (lev > cap) continue;
      if (tierFilter && !tierFilter.has(lev)) continue;
      out.push({
        base: sym.base,
        lev,
        symbol: `${sym.base}-${lev}X`,
        basePriceUsd: sym.basePriceUsd,
        class: sym.class,
      });
    }
  }
  return out;
}

/** Group a market list by base so all tiers share one price account. */
function groupByBase(markets) {
  const groups = new Map();
  for (const m of markets) {
    if (!groups.has(m.base)) groups.set(m.base, { base: m.base, basePriceUsd: m.basePriceUsd, markets: [] });
    groups.get(m.base).markets.push(m);
  }
  return [...groups.values()];
}

function loadOrCreatePriceKeypairFor(base) {
  const path = priceKeypairPathFor(base);
  if (existsSync(path)) {
    return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))));
  }
  const kp = Keypair.generate();
  writeFileSync(path, JSON.stringify(Array.from(kp.secretKey)));
  console.log(`[keeper] generated price account keypair (${base}) → ${path}`);
  return kp;
}

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

function marketParams(symbol16, leverageBps) {
  return {
    symbol: Array.from(symbol16),
    leverageBps,
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
  const [vaultAuthority] = PublicKey.findProgramAddressSync(
    [Buffer.from("market_vault_authority"), market.toBuffer()], MOLE,
  );
  const [subPool] = PublicKey.findProgramAddressSync(
    [Buffer.from("sub_pool"), market.toBuffer(), u32le(0)], MOLE,
  );
  const [longLedger] = PublicKey.findProgramAddressSync(
    [Buffer.from("dist_ledger"), subPool.toBuffer(), Buffer.from([1])], MOLE,
  );
  const [shortLedger] = PublicKey.findProgramAddressSync(
    [Buffer.from("dist_ledger"), subPool.toBuffer(), Buffer.from([0])], MOLE,
  );
  return { globalConfig, market, vault, feeVault, vaultAuthority, subPool, longLedger, shortLedger };
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

// One price account per base, owned by the mock oracle program, seeded at the
// catalog reference price. Shared by every leverage tier of that base.
async function ensurePriceAccount(conn, wallet, priceKp, seedUsd, base) {
  if (await conn.getAccountInfo(priceKp.publicKey)) {
    console.log(`[keeper] price account exists (${base}): ${priceKp.publicKey.toBase58()}`);
    return;
  }
  const rent = await conn.getMinimumBalanceForRentExemption(PRICE_ACCOUNT_SPACE);
  const createIx = SystemProgram.createAccount({
    fromPubkey: wallet.publicKey,
    newAccountPubkey: priceKp.publicKey,
    lamports: rent,
    space: PRICE_ACCOUNT_SPACE,
    programId: MOCK_ORACLE,
  });
  const seedRaw = Math.round(seedUsd * PRICE_SCALE);
  const conf = Math.max(1, Math.round(seedRaw * 0.0005)); // ~0.05%
  const seedIx = new TransactionInstruction({
    programId: MOCK_ORACLE,
    keys: [acc(priceKp.publicKey, false, true)],
    data: setPriceData(seedRaw, conf),
  });
  const sig = await sendTx(conn, wallet, [createIx, seedIx], [priceKp]);
  console.log(`[keeper] price account created + seeded ${base} ($${seedUsd}): ${priceKp.publicKey.toBase58()}  ${sig}`);
}

// Idempotently create one leverage-tier Market (+ vaults, sub-pool, ledgers)
// wired to the base's shared oracle. leverageBps = leverage * 10_000.
async function setupMarket(conn, wallet, priceKp, pdas, symbol16, leverageBps, label) {
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
      data: ixData("initialize_market", INIT_MARKET_PARAMS, marketParams(symbol16, leverageBps)),
    });
    console.log(`[keeper] initialize_market ${label}: ${await sendTx(conn, wallet, [ix])}`);
  } else {
    console.log(`[keeper] market exists (${label}): ${pdas.market.toBase58()}`);
  }

  // market token vaults — open_position settles collateral into these.
  if (!(await conn.getAccountInfo(pdas.vault))) {
    const ix = new TransactionInstruction({
      programId: MOLE,
      keys: [
        acc(pdas.market, false, false),
        acc(pdas.globalConfig, false, false),
        acc(COLLATERAL_MINT, false, false),
        acc(pdas.vaultAuthority, false, false),
        acc(pdas.vault, false, true),
        acc(pdas.feeVault, false, true),
        acc(wallet.publicKey, true, true), // admin (payer)
        acc(TOKEN_PROGRAM, false, false),
        acc(SystemProgram.programId, false, false),
        acc(SYSVAR_RENT_PUBKEY, false, false),
      ],
      data: ixData("initialize_market_vaults"),
    });
    console.log(`[keeper] initialize_market_vaults ${label}: ${await sendTx(conn, wallet, [ix])}`);
  } else {
    console.log(`[keeper] vaults exist (${label}): ${pdas.vault.toBase58()}`);
  }

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
    console.log(`[keeper] initialize_sub_pool 0 ${label}: ${await sendTx(conn, wallet, [ix])}`);
  } else {
    console.log(`[keeper] sub_pool exists (${label}): ${pdas.subPool.toBase58()}`);
  }

  for (const [dir, ledger, dlabel] of [[true, pdas.longLedger, "long"], [false, pdas.shortLedger, "short"]]) {
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
      console.log(`[keeper] init ledger (${dlabel}) ${label}: ${await sendTx(conn, wallet, [ix])}`);
    } else {
      console.log(`[keeper] ${dlabel} ledger exists (${label}): ${ledger.toBase58()}`);
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

function baseSeed(base) {
  let h = 0;
  for (let i = 0; i < base.length; i += 1) h = (h * 31 + base.charCodeAt(i)) >>> 0;
  return (h % 1000) / 1000;
}

const SYNC_CHUNK = 4; // markets per tx (keeps the tx under the size limit)

// Per-tick: for every base push one fresh price, then sync each of its
// leverage-tier markets. Syncs are chunked so each tx stays well under the
// 1232-byte limit; set_price rides in the first chunk so age stays ~0.
async function runGroups(conn, wallet, groups) {
  const total = groups.reduce((n, g) => n + g.markets.length, 0);
  console.log(`[keeper] loop every ${INTERVAL_MS}ms — ${total} markets across ${groups.length} bases (Ctrl-C to stop)`);
  let tick = 0;
  let stop = false;
  process.on("SIGINT", () => { stop = true; console.log("\n[keeper] stopping…"); });
  while (!stop) {
    for (const g of groups) {
      const phase = baseSeed(g.base) * Math.PI * 2;
      const wiggle = Math.sin(tick / 3 + phase) * 0.004 + (Math.random() - 0.5) * 0.001;
      const price = Math.max(1, Math.round(g.basePriceUsd * (1 + wiggle) * PRICE_SCALE));
      const conf = Math.max(1, Math.round(price * 0.0005));
      const setIx = buildSetPriceIx(g.priceKp, price, conf);
      const syncIxs = g.markets.map((m) => buildSyncIx(m.pdas, g.priceKp));
      try {
        for (let i = 0; i < syncIxs.length; i += SYNC_CHUNK) {
          const chunk = syncIxs.slice(i, i + SYNC_CHUNK);
          await sendTx(conn, wallet, i === 0 ? [setIx, ...chunk] : chunk);
        }
        console.log(`[keeper] tick ${tick} ${g.base}: $${(price / PRICE_SCALE).toFixed(4)} → ${g.markets.length} markets`);
      } catch (e) {
        console.warn(`[keeper] tick ${tick} ${g.base} failed: ${e.message}`);
      }
    }
    tick += 1;
    await new Promise((r) => setTimeout(r, INTERVAL_MS));
  }
}

async function main() {
  const mode = process.argv[2] || "all";
  const wallet = loadWallet();
  const conn = new Connection(RPC_URL, "confirmed");
  const catalog = loadCatalog();
  const markets = buildMarketList(catalog);
  const groups = groupByBase(markets);
  for (const g of groups) {
    g.priceKp = loadOrCreatePriceKeypairFor(g.base);
    for (const m of g.markets) {
      m.symbol16 = symbolBytes16(m.symbol);
      m.pdas = derivePdas(m.symbol16);
    }
  }

  console.log("[keeper] rpc:        " + RPC_URL.replace(/api-key=[^&]+/, "api-key=***"));
  console.log("[keeper] mole:       " + MOLE.toBase58());
  console.log("[keeper] mockOracle: " + MOCK_ORACLE.toBase58());
  console.log("[keeper] wallet:     " + wallet.publicKey.toBase58());
  console.log(`[keeper] markets:    ${markets.length} across ${groups.length} bases`);
  console.log("[keeper] bases:      " + groups.map((g) => `${g.base}(${g.markets.length})`).join(", "));

  if (mode === "setup" || mode === "all") {
    for (const g of groups) {
      await ensurePriceAccount(conn, wallet, g.priceKp, g.basePriceUsd, g.base);
      for (const m of g.markets) {
        await setupMarket(conn, wallet, g.priceKp, m.pdas, m.symbol16, m.lev * 10_000, m.symbol);
      }
    }
  }
  if (mode === "run" || mode === "all") await runGroups(conn, wallet, groups);

  if (mode === "setup") {
    const entries = markets.map((m) => ({
      symbol: m.symbol,
      programId: MOLE.toBase58(),
      marketPda: m.pdas.market.toBase58(),
    }));
    const json = JSON.stringify(entries);
    // Auto-write VITE_MARKETS into frontend/.env.local so the operator
    // doesn't have to hand-copy it. Replaces any existing VITE_MARKETS line,
    // preserves all other env keys.
    try {
      const envPath = join(HERE, "..", ".env.local");
      let env = existsSync(envPath) ? readFileSync(envPath, "utf8") : "";
      env = env.replace(/^VITE_MARKETS=.*$\n?/gm, "");
      if (env.length && !env.endsWith("\n")) env += "\n";
      env += `VITE_MARKETS=${json}\n`;
      writeFileSync(envPath, env);
      console.log(`\n[keeper] setup done. Wrote VITE_MARKETS (${entries.length} markets) → ${envPath}`);
    } catch (e) {
      console.log("\n[keeper] setup done. VITE_MARKETS entries:");
      console.log(json);
      console.warn(`[keeper] could not auto-write .env.local: ${e.message}`);
    }
    console.log("Next:  rebuild frontend, then run the loop:  node frontend/scripts/keeper-devnet.mjs run");
  }
}

main().catch((e) => { console.error("[keeper] FAILED:", e?.message || e); process.exit(1); });
