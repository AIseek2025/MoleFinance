// Real on-chain transaction assembly for open_position.
//
// The wave-15 trader path only built the Anchor *instruction data*
// (8-byte discriminator + Borsh args) and handed those raw bytes to the
// wallet. That is not a submittable Solana transaction. This module wraps
// that instruction data into a full legacy `Transaction` with the correct
// account metas (mirrors `programs/mole-option/src/instructions/open.rs`),
// the user's associated USDC token account, derived PDAs, a fee payer,
// and a fresh blockhash — then serializes it for the wallet to sign+send.
//
// Collateral unit: the market settles in its SPL `collateral_mint`
// (devnet USDC, 6 decimals) via anchor_spl::token::Transfer — NOT native
// SOL — so amounts are micro-USDC (1e6 minor units).

import {
  Connection,
  PublicKey,
  Transaction,
  TransactionInstruction,
  SystemProgram,
} from "@solana/web3.js";
import {
  getAssociatedTokenAddressSync,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";

/** Canonical Solana devnet USDC mint (6 decimals). */
export const DEVNET_USDC_MINT = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";

export interface LiveConfig {
  rpcUrl: string;
  programId: PublicKey;
  marketPda: PublicKey;
  collateralMint: PublicKey;
}

/** Read the live on-chain config from Vite env, or null when unset. */
export function readLiveConfig(): LiveConfig | null {
  const env = (import.meta as unknown as {
    env?: Record<string, string | undefined>;
  }).env;
  const rpcUrl = env?.VITE_RPC_URL;
  const programIdRaw = env?.VITE_MOLE_PROGRAM_ID;
  const marketPdaRaw = env?.VITE_MARKET_PDA;
  if (!rpcUrl || !programIdRaw || !marketPdaRaw) return null;
  try {
    return {
      rpcUrl,
      programId: new PublicKey(programIdRaw),
      marketPda: new PublicKey(marketPdaRaw),
      collateralMint: new PublicKey(env?.VITE_COLLATERAL_MINT ?? DEVNET_USDC_MINT),
    };
  } catch {
    return null;
  }
}

function u32le(n: number): Buffer {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(n >>> 0, 0);
  return b;
}
function u64le(n: bigint): Buffer {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(n, 0);
  return b;
}

function ownerKeyFromHex(ownerHex: string): PublicKey {
  return new PublicKey(Buffer.from(ownerHex, "hex"));
}

/**
 * Derive every PDA / token account the `open_position` ix needs, mirroring
 * the seeds in the on-chain program and `keeper-devnet.mjs`.
 */
export function deriveOpenAccounts(
  cfg: LiveConfig,
  ownerHex: string,
  subPoolId: number,
  positionId: bigint,
) {
  const owner = ownerKeyFromHex(ownerHex);
  const { programId, marketPda: market, collateralMint } = cfg;
  const [vault] = PublicKey.findProgramAddressSync(
    [Buffer.from("vault"), market.toBuffer()],
    programId,
  );
  const [feeVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("fee_vault"), market.toBuffer()],
    programId,
  );
  const [subPool] = PublicKey.findProgramAddressSync(
    [Buffer.from("sub_pool"), market.toBuffer(), u32le(subPoolId)],
    programId,
  );
  const [position] = PublicKey.findProgramAddressSync(
    [Buffer.from("position"), market.toBuffer(), owner.toBuffer(), u64le(positionId)],
    programId,
  );
  const userTokenAccount = getAssociatedTokenAddressSync(collateralMint, owner);
  return { owner, market, vault, feeVault, subPool, position, userTokenAccount };
}

/**
 * Build the `open_position` TransactionInstruction with account metas in
 * the exact order Anchor expects (see `OpenPosition` accounts struct).
 */
export function buildOpenInstruction(
  cfg: LiveConfig,
  ownerHex: string,
  subPoolId: number,
  positionId: bigint,
  instructionData: Uint8Array,
): TransactionInstruction {
  const a = deriveOpenAccounts(cfg, ownerHex, subPoolId, positionId);
  return new TransactionInstruction({
    programId: cfg.programId,
    keys: [
      { pubkey: a.market, isSigner: false, isWritable: true },
      { pubkey: a.subPool, isSigner: false, isWritable: true },
      { pubkey: a.position, isSigner: false, isWritable: true },
      { pubkey: a.vault, isSigner: false, isWritable: true },
      { pubkey: a.feeVault, isSigner: false, isWritable: true },
      { pubkey: a.userTokenAccount, isSigner: false, isWritable: true },
      { pubkey: a.owner, isSigner: true, isWritable: true },
      { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    data: Buffer.from(instructionData),
  });
}

/**
 * Assemble + serialize a full, submittable `open_position` transaction.
 * Returns the serialized (unsigned) legacy transaction bytes the wallet
 * adapter forwards to the provider's signAndSendTransaction.
 */
export async function buildOpenTransaction(params: {
  cfg: LiveConfig;
  ownerHex: string;
  subPoolId: number;
  positionId: bigint;
  instructionData: Uint8Array;
}): Promise<Uint8Array> {
  const { cfg, ownerHex, subPoolId, positionId, instructionData } = params;
  const conn = new Connection(cfg.rpcUrl, "confirmed");
  const owner = ownerKeyFromHex(ownerHex);
  const ix = buildOpenInstruction(cfg, ownerHex, subPoolId, positionId, instructionData);
  const tx = new Transaction().add(ix);
  tx.feePayer = owner;
  const { blockhash } = await conn.getLatestBlockhash("confirmed");
  tx.recentBlockhash = blockhash;
  return tx.serialize({ requireAllSignatures: false, verifySignatures: false });
}
