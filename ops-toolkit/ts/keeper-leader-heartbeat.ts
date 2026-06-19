// Wave 17 — KL-08: Manually publish a `keeper_leader_heartbeat`.
//
// Mostly used for ops debugging — the keeper-bot publishes
// heartbeats automatically on its tick cadence. Useful when the
// operator wants to:
//   • Verify a wallet has CU / lamports to land a heartbeat
//   • Manually refresh a lock that's about to go stale during a
//     planned RPC migration
//   • Drive the wave-16 program-test reject-matrix from a real
//     cluster (e.g. observed_slot < last_heartbeat_slot)

import { PublicKey, Transaction } from "@solana/web3.js";

import {
  buildKeeperLeaderHeartbeatIx,
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
  const keeperPath = requireFlag(flags, "keeper");
  const keeper = loadKeypair(keeperPath);
  const conn = connectionFromArgs(maybeFlag(flags, "rpc"));
  const { pda: lockPda } = deriveKeeperLeaderLockPda(programId, market);

  const observedSlotFlag = maybeFlag(flags, "observed-slot");
  const observedSlot = observedSlotFlag
    ? BigInt(observedSlotFlag)
    : BigInt(await conn.getSlot("confirmed"));

  console.log("[keeper-leader-heartbeat] derived");
  console.log(`  program       : ${programId.toBase58()}`);
  console.log(`  market        : ${market.toBase58()}`);
  console.log(`  lockPda       : ${lockPda.toBase58()}`);
  console.log(`  keeper        : ${keeper.publicKey.toBase58()}`);
  console.log(`  observedSlot  : ${observedSlot}`);

  const ix = buildKeeperLeaderHeartbeatIx({
    programId,
    market,
    lockPda,
    keeper: keeper.publicKey,
    observedSlot,
  });
  console.log(
    `[keeper-leader-heartbeat] ix data (hex, ${ix.data.length} bytes): ${ix.data.toString("hex")}`,
  );

  if (!flags.has("confirm")) {
    console.log("[keeper-leader-heartbeat] dry-run — pass --confirm to submit");
    return;
  }

  const tx = new Transaction().add(ix);
  tx.feePayer = keeper.publicKey;
  const { blockhash } = await conn.getLatestBlockhash();
  tx.recentBlockhash = blockhash;
  tx.sign(keeper);
  const sig = await conn.sendRawTransaction(tx.serialize());
  await conn.confirmTransaction(sig, "confirmed");
  console.log(`[keeper-leader-heartbeat] submitted tx: ${sig}`);
}

function requireEnv(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`missing env ${name} (or pass the equivalent flag)`);
  return v;
}

function printUsage(): void {
  console.log(`Usage: keeper-leader-heartbeat [flags]

Required:
  --market         <pubkey>       Market PDA the lock belongs to
  --keeper         <keypair.json> Wallet to record as the holder

Optional:
  --observed-slot  <slot>         Slot to record (default: cluster current)
  --rpc            <url>          (or MOLE_RPC_URL env)
  --program        <pubkey>       (or MOLE_PROGRAM_ID env)
  --confirm                       Submit the tx (default is dry-run)
  --help                          Show this help and exit
`);
}

main().catch((e) => {
  console.error(
    `[keeper-leader-heartbeat] fatal: ${e instanceof Error ? e.message : e}`,
  );
  process.exit(1);
});
