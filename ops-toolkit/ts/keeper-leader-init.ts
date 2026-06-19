// Wave 17 — KL-01: Initialize the per-market `KeeperLeaderLock` PDA.
//
// Runbook reference: Docs/Planning/24-operator-runbook.md §6.5.2.
//
// Usage:
//   npx ts-node keeper-leader-init.ts \
//     --rpc "$MOLE_RPC_URL" \
//     --program "$MOLE_PROGRAM_ID" \
//     --market "$MARKET_PDA" \
//     --payer ~/.config/solana/keeper-hot.json \
//     [--confirm]
//
// Without `--confirm` the script is a dry-run: it derives the PDA,
// prints the encoded instruction bytes + accounts, and exits without
// touching the cluster.

import { PublicKey, Transaction } from "@solana/web3.js";

import {
  buildInitializeKeeperLeaderLockIx,
  connectionFromArgs,
  deriveKeeperLeaderLockPda,
  loadKeypair,
  maybeFlag,
  parseFlags,
  requireFlag,
} from "./lib.js";

async function main(): Promise<void> {
  const flags = parseFlags(process.argv.slice(2));
  if (flags.has("help") || flags.has("h")) {
    printUsage();
    return;
  }
  const programId = new PublicKey(
    maybeFlag(flags, "program") ?? requireEnv("MOLE_PROGRAM_ID"),
  );
  const market = new PublicKey(requireFlag(flags, "market"));
  const payerPath = requireFlag(flags, "payer");
  const payer = loadKeypair(payerPath);
  const conn = connectionFromArgs(maybeFlag(flags, "rpc"));
  const { pda: lockPda } = deriveKeeperLeaderLockPda(programId, market);

  console.log("[keeper-leader-init] derived");
  console.log(`  program     : ${programId.toBase58()}`);
  console.log(`  market      : ${market.toBase58()}`);
  console.log(`  lockPda     : ${lockPda.toBase58()}`);
  console.log(`  payer       : ${payer.publicKey.toBase58()}`);

  // Pre-flight: the PDA might already exist from a previous KL-01
  // run. Initialising twice would fail at the program level
  // (Anchor `init` rejects existing accounts), so we surface this
  // up front with a friendly error instead of letting the cluster
  // throw a cryptic Anchor message.
  const existing = await conn.getAccountInfo(lockPda);
  if (existing) {
    console.error(
      `[keeper-leader-init] PDA ${lockPda.toBase58()} already exists ` +
        `(${existing.data.length} bytes). Use keeper-leader-show.ts to ` +
        `inspect; init is a one-shot.`,
    );
    process.exit(1);
  }

  const ix = buildInitializeKeeperLeaderLockIx({
    programId,
    market,
    lockPda,
    payer: payer.publicKey,
  });
  console.log(
    `[keeper-leader-init] ix data (hex, ${ix.data.length} bytes): ${ix.data.toString("hex")}`,
  );

  if (!flags.has("confirm")) {
    console.log("[keeper-leader-init] dry-run — pass --confirm to submit");
    return;
  }

  const tx = new Transaction().add(ix);
  tx.feePayer = payer.publicKey;
  const { blockhash } = await conn.getLatestBlockhash();
  tx.recentBlockhash = blockhash;
  tx.sign(payer);
  const sig = await conn.sendRawTransaction(tx.serialize());
  await conn.confirmTransaction(sig, "confirmed");
  console.log(`[keeper-leader-init] submitted tx: ${sig}`);
}

function requireEnv(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`missing env ${name} (or pass the equivalent flag)`);
  return v;
}

function printUsage(): void {
  console.log(`Usage: keeper-leader-init [flags]

Required:
  --market <pubkey>        Market PDA the lock belongs to
  --payer  <keypair.json>  Wallet that pays rent for the new account

Optional:
  --rpc     <url>          (or MOLE_RPC_URL env)
  --program <pubkey>       (or MOLE_PROGRAM_ID env)
  --confirm                Submit the tx (default is dry-run)
  --help                   Show this help and exit
`);
}

main().catch((e) => {
  console.error(`[keeper-leader-init] fatal: ${e instanceof Error ? e.message : e}`);
  process.exit(1);
});
