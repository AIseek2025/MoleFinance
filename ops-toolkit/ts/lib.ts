// Wave 17 — shared helpers for the keeper-leader CLI scripts.
//
// Pure, side-effect-free utilities used by every `keeper-leader-*.ts`
// entrypoint. The CLI binaries are kept thin so the bulk of the
// logic is testable.

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

import {
  Connection,
  Keypair,
  PublicKey,
  TransactionInstruction,
  type AccountMeta,
} from "@solana/web3.js";

// ---------------------------------------------------------------------
// Anchor discriminators
// ---------------------------------------------------------------------

/** `sha256("global:<ix_name>")[..8]`. Mirrors `keeper_decoder::ix::instruction_discriminator`. */
export function instructionDiscriminator(ixName: string): Buffer {
  const h = createHash("sha256");
  h.update("global:");
  h.update(ixName);
  return h.digest().subarray(0, 8);
}

/** `sha256("account:<TypeName>")[..8]`. Mirrors `keeper_decoder::ix::account_discriminator`. */
export function accountDiscriminator(typeName: string): Buffer {
  const h = createHash("sha256");
  h.update("account:");
  h.update(typeName);
  return h.digest().subarray(0, 8);
}

// ---------------------------------------------------------------------
// PDA derivation
// ---------------------------------------------------------------------

export const KEEPER_LEADER_LOCK_SEED = Buffer.from("keeper_leader_lock");

/** Derive `[program, [b"keeper_leader_lock", market]]` PDA. */
export function deriveKeeperLeaderLockPda(
  programId: PublicKey,
  marketPda: PublicKey,
): { pda: PublicKey; bump: number } {
  const [pda, bump] = PublicKey.findProgramAddressSync(
    [KEEPER_LEADER_LOCK_SEED, marketPda.toBytes()],
    programId,
  );
  return { pda, bump };
}

// ---------------------------------------------------------------------
// Instruction body encoders (Borsh args after the 8-byte disc)
// ---------------------------------------------------------------------

function encodeU64Le(value: bigint): Buffer {
  const buf = Buffer.alloc(8);
  buf.writeBigUInt64LE(value);
  return buf;
}

/** `KeeperLeaderHeartbeatArgs { observed_slot: u64 }` — 8 bytes. */
export function encodeKeeperLeaderHeartbeatArgs(observedSlot: bigint): Buffer {
  return Buffer.concat([
    instructionDiscriminator("keeper_leader_heartbeat"),
    encodeU64Le(observedSlot),
  ]);
}

/** Same on-chain ix-context as heartbeat; different discriminator. */
export function encodeKeeperLeaderAcquireArgs(observedSlot: bigint): Buffer {
  return Buffer.concat([
    instructionDiscriminator("keeper_leader_acquire"),
    encodeU64Le(observedSlot),
  ]);
}

/** `KeeperLeaderReleaseArgs {}` — empty body, just the 8-byte disc. */
export function encodeKeeperLeaderReleaseArgs(): Buffer {
  return Buffer.from(instructionDiscriminator("keeper_leader_release"));
}

/** `initialize_keeper_leader_lock` — empty body, just the 8-byte disc. */
export function encodeInitializeKeeperLeaderLockArgs(): Buffer {
  return Buffer.from(instructionDiscriminator("initialize_keeper_leader_lock"));
}

// ---------------------------------------------------------------------
// Instruction builders
// ---------------------------------------------------------------------

/** `initialize_keeper_leader_lock` — accounts: [market, lock, payer (signer+mut), system_program]. */
export function buildInitializeKeeperLeaderLockIx(args: {
  programId: PublicKey;
  market: PublicKey;
  lockPda: PublicKey;
  payer: PublicKey;
}): TransactionInstruction {
  const accounts: AccountMeta[] = [
    { pubkey: args.market, isSigner: false, isWritable: false },
    { pubkey: args.lockPda, isSigner: false, isWritable: true },
    { pubkey: args.payer, isSigner: true, isWritable: true },
    {
      pubkey: new PublicKey("11111111111111111111111111111111"),
      isSigner: false,
      isWritable: false,
    },
  ];
  return new TransactionInstruction({
    programId: args.programId,
    keys: accounts,
    data: encodeInitializeKeeperLeaderLockArgs(),
  });
}

/** `keeper_leader_heartbeat` — accounts: [market, lock (mut), keeper (signer)]. */
export function buildKeeperLeaderHeartbeatIx(args: {
  programId: PublicKey;
  market: PublicKey;
  lockPda: PublicKey;
  keeper: PublicKey;
  observedSlot: bigint;
}): TransactionInstruction {
  const accounts: AccountMeta[] = [
    { pubkey: args.market, isSigner: false, isWritable: false },
    { pubkey: args.lockPda, isSigner: false, isWritable: true },
    { pubkey: args.keeper, isSigner: true, isWritable: false },
  ];
  return new TransactionInstruction({
    programId: args.programId,
    keys: accounts,
    data: encodeKeeperLeaderHeartbeatArgs(args.observedSlot),
  });
}

/** `keeper_leader_acquire` — same accounts as heartbeat. */
export function buildKeeperLeaderAcquireIx(args: {
  programId: PublicKey;
  market: PublicKey;
  lockPda: PublicKey;
  keeper: PublicKey;
  observedSlot: bigint;
}): TransactionInstruction {
  const accounts: AccountMeta[] = [
    { pubkey: args.market, isSigner: false, isWritable: false },
    { pubkey: args.lockPda, isSigner: false, isWritable: true },
    { pubkey: args.keeper, isSigner: true, isWritable: false },
  ];
  return new TransactionInstruction({
    programId: args.programId,
    keys: accounts,
    data: encodeKeeperLeaderAcquireArgs(args.observedSlot),
  });
}

/** `keeper_leader_release` — same accounts as heartbeat / acquire. */
export function buildKeeperLeaderReleaseIx(args: {
  programId: PublicKey;
  market: PublicKey;
  lockPda: PublicKey;
  keeper: PublicKey;
}): TransactionInstruction {
  const accounts: AccountMeta[] = [
    { pubkey: args.market, isSigner: false, isWritable: false },
    { pubkey: args.lockPda, isSigner: false, isWritable: true },
    { pubkey: args.keeper, isSigner: true, isWritable: false },
  ];
  return new TransactionInstruction({
    programId: args.programId,
    keys: accounts,
    data: encodeKeeperLeaderReleaseArgs(),
  });
}

// ---------------------------------------------------------------------
// On-chain `KeeperLeaderLock` decoder
// ---------------------------------------------------------------------

export interface KeeperLeaderLockView {
  hasLeader: boolean;
  currentLeader: Buffer;
  lastHeartbeatSlot: bigint;
  takeoverThresholdSlots: bigint;
}

/** Decode the 57-byte (8 disc + 49 body) account data. Throws on malformed input. */
export function decodeKeeperLeaderLockAccount(data: Buffer): KeeperLeaderLockView {
  const expectedDisc = accountDiscriminator("KeeperLeaderLock");
  if (data.length < 8 + 49) {
    throw new Error(
      `KeeperLeaderLock account too short: got ${data.length} bytes, expected >=57`,
    );
  }
  const disc = data.subarray(0, 8);
  if (!disc.equals(expectedDisc)) {
    throw new Error(
      `KeeperLeaderLock discriminator mismatch: got ${disc.toString("hex")}, ` +
        `expected ${expectedDisc.toString("hex")}`,
    );
  }
  const body = data.subarray(8);
  // Layout: bool (1) + [u8; 32] + u64 + u64 = 1 + 32 + 8 + 8 = 49.
  const hasLeader = body[0] === 1;
  const currentLeader = Buffer.from(body.subarray(1, 1 + 32));
  const lastHeartbeatSlot = body.readBigUInt64LE(1 + 32);
  const takeoverThresholdSlots = body.readBigUInt64LE(1 + 32 + 8);
  return {
    hasLeader,
    currentLeader,
    lastHeartbeatSlot,
    takeoverThresholdSlots,
  };
}

// ---------------------------------------------------------------------
// Keypair / RPC helpers
// ---------------------------------------------------------------------

/** Read a Solana CLI keypair JSON file (array of 64 bytes). */
export function loadKeypair(path: string): Keypair {
  const raw = readFileSync(path, "utf-8");
  const parsed = JSON.parse(raw) as number[];
  if (!Array.isArray(parsed) || parsed.length !== 64) {
    throw new Error(
      `Keypair file ${path} must be a JSON array of 64 bytes; got ${typeof parsed}`,
    );
  }
  return Keypair.fromSecretKey(Uint8Array.from(parsed));
}

/**
 * Build a `Connection` from `--rpc` flag or `MOLE_RPC_URL` env. Throws
 * a friendly error when neither is set so the operator gets a clear
 * remediation prompt.
 */
export function connectionFromArgs(rpcFlag: string | undefined): Connection {
  const url = rpcFlag ?? process.env.MOLE_RPC_URL;
  if (!url) {
    throw new Error(
      "no RPC URL provided. Pass --rpc <url> or set MOLE_RPC_URL=<url>.",
    );
  }
  return new Connection(url, "confirmed");
}

// ---------------------------------------------------------------------
// Tiny CLI flag parser
// ---------------------------------------------------------------------

/**
 * Minimal `--flag value` parser. Sufficient for the wave-17 ops
 * scripts; refusing larger features (sub-commands, equals-form,
 * negation) on purpose so the parser stays one-page-readable.
 */
export function parseFlags(argv: string[]): Map<string, string | true> {
  const out = new Map<string, string | true>();
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]!;
    if (!a.startsWith("--")) continue;
    const key = a.slice(2);
    const next = argv[i + 1];
    if (next === undefined || next.startsWith("--")) {
      out.set(key, true);
    } else {
      out.set(key, next);
      i += 1;
    }
  }
  return out;
}

export function requireFlag(flags: Map<string, string | true>, key: string): string {
  const v = flags.get(key);
  if (v === undefined || v === true) {
    throw new Error(`missing required flag --${key}`);
  }
  return v;
}

export function maybeFlag(
  flags: Map<string, string | true>,
  key: string,
): string | undefined {
  const v = flags.get(key);
  return v === undefined || v === true ? undefined : v;
}

export function shortenHex(buf: Buffer | Uint8Array): string {
  if (buf.length < 8) return Buffer.from(buf).toString("hex");
  const head = Buffer.from(buf.subarray(0, 4)).toString("hex");
  const tail = Buffer.from(buf.subarray(buf.length - 4)).toString("hex");
  return `${head}…${tail}`;
}

// ---------------------------------------------------------------------
// Wave 18 — multi-market TOML registry parser
// ---------------------------------------------------------------------
//
// Mirrors the Rust `keeper_rpc::market_registry::MarketRegistry::from_toml_str`
// subset: `[[markets]]` array of tables with bare-key string values
// only. Keeping the format identical means ops can ship one
// `markets.toml` and have BOTH the Rust prober and the TS CLI
// scripts read it without re-encoding.

/** Wave 18 — one parsed market entry. */
export interface MarketRegistryEntry {
  symbol: string;
  programId: PublicKey;
  marketPda: PublicKey;
  /** Pinned PDA (when supplied) — otherwise derived from programId+marketPda. */
  lockPda: PublicKey;
  /** Optional operator-authoritative expected leader pubkey. */
  expectedLeader?: PublicKey;
}

/**
 * Wave 19 — substitute `${VAR}` placeholders against `lookup`.
 * `$$` becomes literal `$`. Variable names must match
 * `[A-Za-z_][A-Za-z0-9_]*`. Mirrors the Rust
 * `keeper_rpc::market_registry::substitute_env_vars` semantics
 * byte-for-byte so a single `markets.toml` flows through Rust +
 * TS without divergence.
 */
export function substituteEnvVars(
  input: string,
  lookup: (name: string) => string | undefined,
): string {
  let out = "";
  let i = 0;
  while (i < input.length) {
    const ch = input[i]!;
    if (ch !== "$") {
      out += ch;
      i += 1;
      continue;
    }
    if (i + 1 < input.length && input[i + 1] === "$") {
      out += "$";
      i += 2;
      continue;
    }
    if (i + 1 < input.length && input[i + 1] === "{") {
      const start = i + 2;
      let end = start;
      while (end < input.length && input[end] !== "}") end += 1;
      if (end >= input.length) {
        throw new Error(
          `markets.toml malformed env ref at byte ${i}: unclosed \${...}`,
        );
      }
      const name = input.slice(start, end);
      if (name.length === 0) {
        throw new Error(
          `markets.toml malformed env ref at byte ${i}: empty variable name \${}`,
        );
      }
      if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
        throw new Error(
          `markets.toml malformed env ref at byte ${i}: variable name '${name}' contains invalid characters (must match [A-Za-z_][A-Za-z0-9_]*)`,
        );
      }
      const v = lookup(name);
      if (v === undefined || v === "") {
        throw new Error(
          `markets.toml environment variable '${name}' is unset or empty (referenced via \${${name}})`,
        );
      }
      out += v;
      i = end + 1;
      continue;
    }
    out += "$";
    i += 1;
  }
  return out;
}

/**
 * Parse a multi-market TOML registry from a string.
 *
 * Throws `Error` with a clear remediation message on any failure:
 * malformed TOML, missing required keys, bad pubkeys, oversized
 * symbols, duplicate symbols.
 *
 * Wave 19 — when `lookup` is supplied, `${VAR}` placeholders are
 * resolved before TOML parsing so the same template can hold
 * SOPS-injected secrets. The default lookup uses `process.env`.
 */
export function parseMarketsToml(
  input: string,
  lookup: (name: string) => string | undefined = (n) => process.env[n],
): MarketRegistryEntry[] {
  const resolved = substituteEnvVars(input, lookup);
  const tables = parseMarketTables(resolved);
  if (tables.length === 0) {
    throw new Error("markets.toml is empty (need at least one [[markets]] entry)");
  }
  const out: MarketRegistryEntry[] = [];
  const seen = new Set<string>();
  for (let i = 0; i < tables.length; i += 1) {
    const t = tables[i]!;
    const symbol = t["symbol"];
    if (typeof symbol !== "string") {
      throw new Error(`markets.toml #${i}: missing required key 'symbol'`);
    }
    if (symbol.length === 0 || symbol.length > 16) {
      throw new Error(
        `markets.toml '${symbol}': symbol must be 1..16 ASCII bytes`,
      );
    }
    if (seen.has(symbol)) {
      throw new Error(`markets.toml: duplicate symbol '${symbol}'`);
    }
    seen.add(symbol);
    const programIdRaw = t["program_id"];
    const marketPdaRaw = t["market_pda"];
    if (typeof programIdRaw !== "string") {
      throw new Error(`markets.toml '${symbol}': missing 'program_id'`);
    }
    if (typeof marketPdaRaw !== "string") {
      throw new Error(`markets.toml '${symbol}': missing 'market_pda'`);
    }
    const programId = parsePubkeyOrFail(symbol, "program_id", programIdRaw);
    const marketPda = parsePubkeyOrFail(symbol, "market_pda", marketPdaRaw);
    let lockPda: PublicKey;
    const lockRaw = t["lock_pda"];
    if (typeof lockRaw === "string" && lockRaw.length > 0) {
      lockPda = parsePubkeyOrFail(symbol, "lock_pda", lockRaw);
    } else {
      const { pda } = deriveKeeperLeaderLockPda(programId, marketPda);
      lockPda = pda;
    }
    let expectedLeader: PublicKey | undefined;
    const expectedRaw = t["expected_leader"];
    if (typeof expectedRaw === "string" && expectedRaw.length > 0) {
      expectedLeader = parsePubkeyOrFail(symbol, "expected_leader", expectedRaw);
    }
    out.push({
      symbol,
      programId,
      marketPda,
      lockPda,
      ...(expectedLeader !== undefined && { expectedLeader }),
    });
  }
  return out;
}

/** Read + parse a `markets.toml` from disk. */
export function loadMarketsToml(path: string): MarketRegistryEntry[] {
  const raw = readFileSync(path, "utf-8");
  return parseMarketsToml(raw);
}

interface RawTable {
  [key: string]: string;
}

function parseMarketTables(input: string): RawTable[] {
  const lines = input.split(/\r?\n/);
  const out: RawTable[] = [];
  let current: RawTable | null = null;
  for (let i = 0; i < lines.length; i += 1) {
    const lineNum = i + 1;
    const line = stripComment(lines[i]!).trim();
    if (line.length === 0) continue;
    if (line.startsWith("[[") && line.endsWith("]]")) {
      const header = line.slice(2, line.length - 2).trim();
      if (header !== "markets") {
        throw new Error(
          `markets.toml line ${lineNum}: unsupported header '[[${header}]]' (only [[markets]] is allowed)`,
        );
      }
      if (current !== null) out.push(current);
      current = {};
      continue;
    }
    if (line.startsWith("[")) {
      throw new Error(
        `markets.toml line ${lineNum}: unsupported header '${line}' (use [[markets]])`,
      );
    }
    const eq = line.indexOf("=");
    if (eq < 0) {
      throw new Error(
        `markets.toml line ${lineNum}: expected 'key = "value"', got '${line}'`,
      );
    }
    const key = line.slice(0, eq).trim();
    if (!/^[A-Za-z0-9_-]+$/.test(key)) {
      throw new Error(
        `markets.toml line ${lineNum}: invalid bare key '${key}'`,
      );
    }
    const rest = line.slice(eq + 1).trim();
    if (!rest.startsWith('"')) {
      throw new Error(
        `markets.toml line ${lineNum}: value for '${key}' must be a double-quoted string (got '${rest}')`,
      );
    }
    const body = rest.slice(1);
    const end = body.indexOf('"');
    if (end < 0) {
      throw new Error(
        `markets.toml line ${lineNum}: unterminated string for '${key}'`,
      );
    }
    const after = body.slice(end + 1).trim();
    if (after.length > 0) {
      throw new Error(
        `markets.toml line ${lineNum}: trailing tokens after string for '${key}': '${after}'`,
      );
    }
    if (current === null) {
      throw new Error(
        `markets.toml line ${lineNum}: orphan key '${key}' outside any [[markets]] table`,
      );
    }
    current[key] = body.slice(0, end);
  }
  if (current !== null) out.push(current);
  return out;
}

function stripComment(line: string): string {
  let inStr = false;
  for (let i = 0; i < line.length; i += 1) {
    const c = line[i]!;
    if (c === '"' && (i === 0 || line[i - 1] !== "\\")) {
      inStr = !inStr;
    } else if (c === "#" && !inStr) {
      return line.slice(0, i);
    }
  }
  return line;
}

function parsePubkeyOrFail(
  symbol: string,
  field: string,
  raw: string,
): PublicKey {
  try {
    return new PublicKey(raw);
  } catch (e) {
    throw new Error(
      `markets.toml '${symbol}': invalid ${field} pubkey '${raw}' — ${(e as Error).message}`,
    );
  }
}
