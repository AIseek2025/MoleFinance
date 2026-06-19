// Wave 17 — KL-03: Force-acquire the keeper-leader lock.
//
// Runbook reference: Docs/Planning/24-operator-runbook.md §6.5.4.
// ONLY use this when the active leader is provably stale (the
// pre-flight enforces `elapsed_slots >= takeover_threshold_slots`).
// For planned handoffs use `keeper-leader-release.ts` from the
// retiring replica + a normal heartbeat from the new leader.

import { PublicKey, Transaction } from "@solana/web3.js";

import {
  buildKeeperLeaderAcquireIx,
  connectionFromArgs,
  decodeKeeperLeaderLockAccount,
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

  // Pre-flight: refuse to even sign the tx unless the lock is
  // verifiably stale. The on-chain ix would reject a fresh-acquire
  // anyway, but a local refusal saves the operator a wasted tx and
  // reduces the chance of a finger-slip during a stressful handoff.
  const info = await conn.getAccountInfo(lockPda);
  if (info) {
    const view = decodeKeeperLeaderLockAccount(info.data);
    if (view.hasLeader) {
      const currentSlot = await conn.getSlot("confirmed");
      const elapsed = BigInt(currentSlot) - view.lastHeartbeatSlot;
      if (elapsed < view.takeoverThresholdSlots) {
        console.error(
          `[keeper-leader-acquire] REFUSED: lock is currently fresh ` +
            `(elapsed=${elapsed} slots, threshold=${view.takeoverThresholdSlots}). ` +
            `Wait for stale or use keeper-leader-release.ts on the active replica.`,
        );
        process.exit(2);
      }
    }
  } else {
    console.error(
      `[keeper-leader-acquire] PDA missing on chain — run keeper-leader-init.ts (KL-01) first`,
    );
    process.exit(2);
  }

  console.log("[keeper-leader-acquire] derived");
  console.log(`  program       : ${programId.toBase58()}`);
  console.log(`  market        : ${market.toBase58()}`);
  console.log(`  lockPda       : ${lockPda.toBase58()}`);
  console.log(`  keeper        : ${keeper.publicKey.toBase58()}`);
  console.log(`  observedSlot  : ${observedSlot}`);

  const ix = buildKeeperLeaderAcquireIx({
    programId,
    market,
    lockPda,
    keeper: keeper.publicKey,
    observedSlot,
  });
  console.log(
    `[keeper-leader-acquire] ix data (hex, ${ix.data.length} bytes): ${ix.data.toString("hex")}`,
  );

  if (!flags.has("confirm")) {
    console.log("[keeper-leader-acquire] dry-run — pass --confirm to submit");
    return;
  }

  const tx = new Transaction().add(ix);
  tx.feePayer = keeper.publicKey;
  const { blockhash } = await conn.getLatestBlockhash();
  tx.recentBlockhash = blockhash;
  tx.sign(keeper);
  const sig = await conn.sendRawTransaction(tx.serialize());
  await conn.confirmTransaction(sig, "confirmed");
  console.log(`[keeper-leader-acquire] submitted tx: ${sig}`);
}

function requireEnv(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`missing env ${name} (or pass the equivalent flag)`);
  return v;
}

function printUsage(): void {
  console.log(`Usage: keeper-leader-acquire [flags]

Required:
  --market         <pubkey>       Market PDA the lock belongs to
  --keeper         <keypair.json> Wallet that wants to take over

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
    `[keeper-leader-acquire] fatal: ${e instanceof Error ? e.message : e}`,
  );
  process.exit(1);
});
