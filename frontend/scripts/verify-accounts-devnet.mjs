// Verify that every on-chain account the frontend's open_position
// transaction depends on actually exists on devnet and is owned by the
// right program. This proves the PDA derivations in
// src/tx/buildOrderTransaction.ts match the live deployment, without
// needing a funded browser wallet.
//
// Run: node scripts/verify-accounts-devnet.mjs

import dns from "node:dns";
dns.setDefaultResultOrder("ipv4first");
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { Connection, PublicKey } from "@solana/web3.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

function readEnvLocal() {
  const file = path.join(__dirname, "..", ".env.local");
  const out = {};
  for (const line of fs.readFileSync(file, "utf8").split("\n")) {
    const m = line.match(/^([A-Z0-9_]+)=(.*)$/);
    if (m) out[m[1]] = m[2].trim();
  }
  return out;
}

function u32le(n) {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(n >>> 0, 0);
  return b;
}

const TOKEN_PROGRAM = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

const env = readEnvLocal();
const RPC = process.env.SOLANA_RPC_URL || env.VITE_RPC_URL;
const programId = new PublicKey(env.VITE_MOLE_PROGRAM_ID);
const market = new PublicKey(env.VITE_MARKET_PDA);
const collateralMint = new PublicKey(
  env.VITE_COLLATERAL_MINT || "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU",
);

const [subPool] = PublicKey.findProgramAddressSync(
  [Buffer.from("sub_pool"), market.toBuffer(), u32le(0)],
  programId,
);
const [globalConfig] = PublicKey.findProgramAddressSync(
  [Buffer.from("global_config")],
  programId,
);

const conn = new Connection(RPC, "confirmed");

// open_position pins `vault` / `fee_vault` / `user_token_account` to the
// values stored *in the Market account* (address = market.vault, etc.), so
// read them from on-chain rather than re-deriving. Market layout (after the
// 8-byte discriminator): global_config[32] symbol[16] collateral_mint[32]
// vault[32] fee_vault[32] oracle_price_feed[32] ...
const marketInfo = await conn.getAccountInfo(market);
let vault = null;
let feeVault = null;
let onchainMint = null;
if (marketInfo) {
  const d = marketInfo.data;
  onchainMint = new PublicKey(d.subarray(56, 88));
  vault = new PublicKey(d.subarray(88, 120));
  feeVault = new PublicKey(d.subarray(120, 152));
  console.log(`market.collateral_mint = ${onchainMint.toBase58()}`);
  console.log(`market.vault           = ${vault.toBase58()}`);
  console.log(`market.fee_vault       = ${feeVault.toBase58()}`);
  console.log(
    `env VITE_COLLATERAL_MINT matches market: ${onchainMint.equals(collateralMint)}\n`,
  );
}

const checks = [
  { name: "program (executable)", key: programId, expectExecutable: true },
  { name: "global_config PDA", key: globalConfig, expectOwner: programId },
  { name: "market PDA", key: market, expectOwner: programId },
  { name: "sub_pool #0 PDA", key: subPool, expectOwner: programId },
  { name: "market.vault token acct", key: vault, expectOwner: TOKEN_PROGRAM },
  { name: "market.fee_vault token acct", key: feeVault, expectOwner: TOKEN_PROGRAM },
  { name: "collateral mint (USDC)", key: collateralMint, expectOwner: TOKEN_PROGRAM },
];

console.log(`RPC: ${RPC.split("?")[0]}`);
console.log(`program: ${programId.toBase58()}`);
console.log(`market:  ${market.toBase58()}\n`);

let ok = 0;
for (const c of checks) {
  const info = await conn.getAccountInfo(c.key);
  let status = "MISSING";
  if (info) {
    if (c.expectExecutable) {
      status = info.executable ? "OK (executable)" : "EXISTS but NOT executable";
      if (info.executable) ok += 1;
    } else if (c.expectOwner) {
      const owned = info.owner.equals(c.expectOwner);
      status = owned
        ? `OK (owner ${c.expectOwner.toBase58().slice(0, 8)}…, ${info.data.length}B)`
        : `WRONG OWNER ${info.owner.toBase58()}`;
      if (owned) ok += 1;
    } else {
      status = "OK";
      ok += 1;
    }
  }
  console.log(`${status.startsWith("OK") ? "✅" : "❌"} ${c.name.padEnd(26)} ${c.key.toBase58()}  ${status}`);
}

console.log(`\n${ok}/${checks.length} account dependencies verified on devnet.`);
process.exit(ok === checks.length ? 0 : 1);
