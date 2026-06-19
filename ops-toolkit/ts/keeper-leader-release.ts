// Wave 17 — KL-02: Manually release the keeper-leader lock.
//
// Runbook reference: Docs/Planning/24-operator-runbook.md §6.5.3.
// The wave-16 keeper-bot also auto-releases on graceful shutdown,
// so this script is mainly for:
//   1. Operators who didn't deploy the wave-16+ bot yet
//   2. Manual handoff while the bot is paused mid-tick
//   3. Programmatic ops automation (Terraform / Ansible playbooks)

import { PublicKey, Transaction } from "@solana/web3.js";

import {
  buildKeeperLeaderReleaseIx,
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

  // Pre-flight: refuse if the signer isn't the current holder. The
  // on-chain ix would reject anyway, but we want a clear local
  // error.
  const info = await conn.getAccountInfo(lockPda);
  if (!info) {
    console.error(
      `[keeper-leader-release] PDA missing on chain — nothing to release`,
    );
    process.exit(2);
  }
  const view = decodeKeeperLeaderLockAccount(info.data);
  if (!view.hasLeader) {
    console.error(
      `[keeper-leader-release] PDA has no current holder — release is a no-op`,
    );
    process.exit(0);
  }
  if (!view.currentLeader.equals(keeper.publicKey.toBuffer())) {
    console.error(
      `[keeper-leader-release] REFUSED: signer ${keeper.publicKey.toBase58()} ` +
        `is NOT the current holder. The on-chain ix only allows release by ` +
        `the current holder; use keeper-leader-acquire.ts (KL-03) if you ` +
        `intend to take over.`,
    );
    process.exit(2);
  }

  console.log("[keeper-leader-release] derived");
  console.log(`  program       : ${programId.toBase58()}`);
  console.log(`  market        : ${market.toBase58()}`);
  console.log(`  lockPda       : ${lockPda.toBase58()}`);
  console.log(`  keeper        : ${keeper.publicKey.toBase58()}`);

  const ix = buildKeeperLeaderReleaseIx({
    programId,
    market,
    lockPda,
    keeper: keeper.publicKey,
  });
  console.log(
    `[keeper-leader-release] ix data (hex, ${ix.data.length} bytes): ${ix.data.toString("hex")}`,
  );

  if (!flags.has("confirm")) {
    console.log("[keeper-leader-release] dry-run — pass --confirm to submit");
    return;
  }

  const tx = new Transaction().add(ix);
  tx.feePayer = keeper.publicKey;
  const { blockhash } = await conn.getLatestBlockhash();
  tx.recentBlockhash = blockhash;
  tx.sign(keeper);
  const sig = await conn.sendRawTransaction(tx.serialize());
  await conn.confirmTransaction(sig, "confirmed");
  console.log(`[keeper-leader-release] submitted tx: ${sig}`);
}

function requireEnv(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`missing env ${name} (or pass the equivalent flag)`);
  return v;
}

function printUsage(): void {
  console.log(`Usage: keeper-leader-release [flags]

Required:
  --market   <pubkey>       Market PDA the lock belongs to
  --keeper   <keypair.json> Current holder's wallet (must match on-chain)

Optional:
  --rpc      <url>          (or MOLE_RPC_URL env)
  --program  <pubkey>       (or MOLE_PROGRAM_ID env)
  --confirm                 Submit the tx (default is dry-run)
  --help                    Show this help and exit
`);
}

main().catch((e) => {
  console.error(
    `[keeper-leader-release] fatal: ${e instanceof Error ? e.message : e}`,
  );
  process.exit(1);
});
