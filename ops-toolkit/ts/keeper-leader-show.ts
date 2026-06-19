// Wave 17 — KL-01..05: Decode + print the on-chain `KeeperLeaderLock`.
//
// Runbook reference: every § 6.5.* SOP uses this script as the
// "verify state" step. Read-only — never submits a tx.
//
// Output is a flat JSON object suitable for piping into `jq` or
// AlertManager. Add `--human` for a colour-coded human-readable
// rendering instead.

import { PublicKey } from "@solana/web3.js";

import {
  connectionFromArgs,
  decodeKeeperLeaderLockAccount,
  deriveKeeperLeaderLockPda,
  maybeFlag,
  parseFlags,
  requireFlag,
  shortenHex,
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
  const conn = connectionFromArgs(maybeFlag(flags, "rpc"));
  const { pda: lockPda } = deriveKeeperLeaderLockPda(programId, market);

  const [info, currentSlot] = await Promise.all([
    conn.getAccountInfo(lockPda),
    conn.getSlot("confirmed"),
  ]);

  if (!info) {
    const out = {
      pda: lockPda.toBase58(),
      initialized: false,
      currentSlot,
      message:
        "PDA does not exist on chain — run keeper-leader-init.ts first (KL-01)",
    };
    print(out, flags.has("human"));
    process.exit(0);
  }

  let view;
  try {
    view = decodeKeeperLeaderLockAccount(info.data);
  } catch (e) {
    console.error(
      `[keeper-leader-show] account decode failed (size=${info.data.length}): ${
        e instanceof Error ? e.message : e
      }`,
    );
    process.exit(2);
  }

  const elapsed = view.hasLeader
    ? Math.max(0, currentSlot - Number(view.lastHeartbeatSlot))
    : 0;
  const stale =
    view.hasLeader && BigInt(elapsed) >= view.takeoverThresholdSlots;

  const out = {
    pda: lockPda.toBase58(),
    initialized: true,
    hasLeader: view.hasLeader,
    currentLeaderHex: view.currentLeader.toString("hex"),
    currentLeaderShort: shortenHex(view.currentLeader),
    lastHeartbeatSlot: view.lastHeartbeatSlot.toString(),
    takeoverThresholdSlots: view.takeoverThresholdSlots.toString(),
    currentSlot,
    elapsedSlots: elapsed,
    stale,
  };
  print(out, flags.has("human"));
}

function print(obj: Record<string, unknown>, human: boolean): void {
  if (!human) {
    console.log(JSON.stringify(obj, null, 2));
    return;
  }
  for (const [k, v] of Object.entries(obj)) {
    console.log(`  ${k.padEnd(24)}: ${v}`);
  }
}

function requireEnv(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`missing env ${name} (or pass the equivalent flag)`);
  return v;
}

function printUsage(): void {
  console.log(`Usage: keeper-leader-show [flags]

Required:
  --market <pubkey>        Market PDA the lock belongs to

Optional:
  --rpc     <url>          (or MOLE_RPC_URL env)
  --program <pubkey>       (or MOLE_PROGRAM_ID env)
  --human                  Pretty-print instead of JSON
  --help                   Show this help and exit
`);
}

main().catch((e) => {
  console.error(`[keeper-leader-show] fatal: ${e instanceof Error ? e.message : e}`);
  process.exit(1);
});
