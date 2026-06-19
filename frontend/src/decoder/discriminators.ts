/**
 * Wave 14 — Anchor account discriminators.
 *
 * Anchor encodes every account with a leading 8-byte discriminator
 * computed as `sha256("account:<TypeName>")[..8]`. The keeper bot's
 * `keeper-rpc::tx` module hard-codes the same constants Rust-side
 * and pins them with a `sha2` self-test
 * (`crates/keeper-rpc/src/tx.rs::tests::
 * discriminator_constants_match_sha256_of_anchor_namespace`).
 *
 * The frontend computes them at module load using `@noble/hashes`
 * (sync, ~3 KB) so we don't need an async startup dance. Test code
 * can call `deriveAnchorAccountDiscriminator` directly to get the
 * canonical bytes for any type name.
 */
import { sha256 } from "@noble/hashes/sha256";

const ENCODER = new TextEncoder();

/**
 * Compute the 8-byte Anchor account discriminator for `typeName`.
 * Mirrors `sha256("account:<typeName>")[..8]` exactly.
 */
export function deriveAnchorAccountDiscriminator(
  typeName: string,
): Uint8Array {
  const input = ENCODER.encode(`account:${typeName}`);
  const digest = sha256(input);
  return digest.slice(0, 8);
}

/** Default mole-option account discriminators. */
export interface MoleAccountDiscriminators {
  market: Uint8Array;
  subPool: Uint8Array;
  dormantBucket: Uint8Array;
  distributionLedger: Uint8Array;
  /** Wave 22 — `Position` PDA discriminator for live position feeds. */
  position: Uint8Array;
}

/**
 * Snapshot of the canonical mole-option account discriminators,
 * computed once at module load. The wave-14 WebSocketFeedAdapter
 * uses these by default; tests inject their own to keep the test
 * surface independent of the on-chain naming convention.
 */
export const MOLE_ACCOUNT_DISCRIMINATORS: MoleAccountDiscriminators = {
  market: deriveAnchorAccountDiscriminator("Market"),
  subPool: deriveAnchorAccountDiscriminator("SubPool"),
  dormantBucket: deriveAnchorAccountDiscriminator("DormantBucket"),
  distributionLedger: deriveAnchorAccountDiscriminator("DistributionLedger"),
  position: deriveAnchorAccountDiscriminator("Position"),
};
