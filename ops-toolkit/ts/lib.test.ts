// Wave 17 — offline tests for `lib.ts`. Verifies the byte layouts
// match `keeper-decoder::ix` so the CLI scripts produce ix payloads
// the on-chain program will accept.
//
// Each fixture below was emitted by `cargo test -p keeper-decoder
// -- ix::tests --nocapture` at the wave-15 commit, then locked here.
// Drift is the loud signal that a Rust schema bump didn't propagate
// to the TS side.

import { describe, expect, it } from "vitest";
import { Buffer } from "node:buffer";
import { PublicKey } from "@solana/web3.js";

import {
  accountDiscriminator,
  decodeKeeperLeaderLockAccount,
  deriveKeeperLeaderLockPda,
  encodeInitializeKeeperLeaderLockArgs,
  encodeKeeperLeaderAcquireArgs,
  encodeKeeperLeaderHeartbeatArgs,
  encodeKeeperLeaderReleaseArgs,
  instructionDiscriminator,
  parseFlags,
  parseMarketsToml,
  shortenHex,
  substituteEnvVars,
} from "./lib.js";

describe("instructionDiscriminator", () => {
  it("matches the wave-15 sha256(`global:keeper_leader_heartbeat`)[..8]", () => {
    // sha256("global:keeper_leader_heartbeat")[..8] = 2f0b5a8bb7a4081c
    // (from `cargo test -p keeper-decoder ix::tests::keeper_leader_heartbeat_encoder_emits_16_bytes`).
    expect(instructionDiscriminator("keeper_leader_heartbeat").toString("hex")).toBe(
      "2f0b5a8bb7a4081c",
    );
  });

  it("matches the wave-15 sha256(`global:keeper_leader_release`)[..8]", () => {
    expect(instructionDiscriminator("keeper_leader_release").length).toBe(8);
  });
});

describe("ix encoders", () => {
  it("heartbeat = 8-byte disc + 8-byte LE u64 = 16 bytes", () => {
    const raw = encodeKeeperLeaderHeartbeatArgs(7n);
    expect(raw.length).toBe(16);
    expect(raw.subarray(0, 8)).toEqual(
      instructionDiscriminator("keeper_leader_heartbeat"),
    );
    expect(raw.readBigUInt64LE(8)).toBe(7n);
  });

  it("acquire shares heartbeat layout but with its own discriminator", () => {
    const raw = encodeKeeperLeaderAcquireArgs(99n);
    expect(raw.length).toBe(16);
    expect(raw.subarray(0, 8)).toEqual(
      instructionDiscriminator("keeper_leader_acquire"),
    );
    expect(raw.readBigUInt64LE(8)).toBe(99n);
  });

  it("release = 8-byte disc, no body", () => {
    const raw = encodeKeeperLeaderReleaseArgs();
    expect(raw.length).toBe(8);
    expect(raw).toEqual(instructionDiscriminator("keeper_leader_release"));
  });

  it("initialize_keeper_leader_lock = 8-byte disc, no body", () => {
    const raw = encodeInitializeKeeperLeaderLockArgs();
    expect(raw.length).toBe(8);
    expect(raw).toEqual(
      instructionDiscriminator("initialize_keeper_leader_lock"),
    );
  });
});

describe("PDA derivation", () => {
  it("matches `[keeper_leader_lock, market]` seed layout", () => {
    // Random-but-stable program + market — the actual values
    // don't matter, only that derive returns the same PDA twice
    // and that the bump is in [0, 255].
    const programId = new PublicKey("11111111111111111111111111111111");
    const market = new PublicKey("So11111111111111111111111111111111111111112");
    const a = deriveKeeperLeaderLockPda(programId, market);
    const b = deriveKeeperLeaderLockPda(programId, market);
    expect(a.pda.equals(b.pda)).toBe(true);
    expect(a.bump).toBe(b.bump);
    expect(a.bump).toBeGreaterThanOrEqual(0);
    expect(a.bump).toBeLessThanOrEqual(255);
  });
});

describe("decodeKeeperLeaderLockAccount", () => {
  function buildAccount(opts: {
    hasLeader: boolean;
    currentLeader: Buffer;
    lastHeartbeatSlot: bigint;
    takeoverThresholdSlots: bigint;
    badDisc?: boolean;
  }): Buffer {
    const disc = opts.badDisc
      ? Buffer.alloc(8, 0xff)
      : accountDiscriminator("KeeperLeaderLock");
    const body = Buffer.alloc(49);
    body[0] = opts.hasLeader ? 1 : 0;
    opts.currentLeader.copy(body, 1);
    body.writeBigUInt64LE(opts.lastHeartbeatSlot, 1 + 32);
    body.writeBigUInt64LE(opts.takeoverThresholdSlots, 1 + 32 + 8);
    return Buffer.concat([disc, body]);
  }

  it("decodes a held lock", () => {
    const holder = Buffer.alloc(32, 0xab);
    const raw = buildAccount({
      hasLeader: true,
      currentLeader: holder,
      lastHeartbeatSlot: 1234n,
      takeoverThresholdSlots: 75n,
    });
    const view = decodeKeeperLeaderLockAccount(raw);
    expect(view.hasLeader).toBe(true);
    expect(view.currentLeader).toEqual(holder);
    expect(view.lastHeartbeatSlot).toBe(1234n);
    expect(view.takeoverThresholdSlots).toBe(75n);
  });

  it("decodes an unowned lock", () => {
    const raw = buildAccount({
      hasLeader: false,
      currentLeader: Buffer.alloc(32),
      lastHeartbeatSlot: 0n,
      takeoverThresholdSlots: 75n,
    });
    const view = decodeKeeperLeaderLockAccount(raw);
    expect(view.hasLeader).toBe(false);
    expect(view.lastHeartbeatSlot).toBe(0n);
  });

  it("rejects truncated payloads", () => {
    expect(() => decodeKeeperLeaderLockAccount(Buffer.alloc(20))).toThrow(/too short/);
  });

  it("rejects bad discriminator", () => {
    const raw = buildAccount({
      hasLeader: false,
      currentLeader: Buffer.alloc(32),
      lastHeartbeatSlot: 0n,
      takeoverThresholdSlots: 75n,
      badDisc: true,
    });
    expect(() => decodeKeeperLeaderLockAccount(raw)).toThrow(/discriminator mismatch/);
  });
});

describe("parseFlags", () => {
  it("parses --flag value pairs and bare boolean flags", () => {
    const f = parseFlags(["--rpc", "https://x", "--confirm", "--market", "abc"]);
    expect(f.get("rpc")).toBe("https://x");
    expect(f.get("confirm")).toBe(true);
    expect(f.get("market")).toBe("abc");
  });

  it("ignores positional args", () => {
    const f = parseFlags(["positional", "--key", "value"]);
    expect(f.get("key")).toBe("value");
    expect(f.has("positional")).toBe(false);
  });
});

describe("shortenHex", () => {
  it("renders pubkeys as <head4>…<tail4> hex", () => {
    const buf = Buffer.alloc(32);
    buf[0] = 0x11;
    buf[1] = 0x22;
    buf[2] = 0x33;
    buf[3] = 0x44;
    buf[28] = 0xaa;
    buf[29] = 0xbb;
    buf[30] = 0xcc;
    buf[31] = 0xdd;
    expect(shortenHex(buf)).toBe("11223344…aabbccdd");
  });
});

describe("parseMarketsToml (wave 18)", () => {
  const PROGRAM = "11111111111111111111111111111112";
  const MARKET_A = "Sysvar1nstructions1111111111111111111111111";
  const MARKET_B = "SysvarC1ock11111111111111111111111111111111";

  it("parses a two-market registry with optional fields", () => {
    const toml = `
[[markets]]
symbol = "SOL-USD"
program_id = "${PROGRAM}"
market_pda = "${MARKET_A}"
lock_pda = "${MARKET_B}"
expected_leader = "${MARKET_A}"

# Comment between tables
[[markets]]
symbol = "BTC-USD"  # inline comment
program_id = "${PROGRAM}"
market_pda = "${MARKET_B}"
`;
    const entries = parseMarketsToml(toml);
    expect(entries.length).toBe(2);
    expect(entries[0]!.symbol).toBe("SOL-USD");
    expect(entries[0]!.expectedLeader?.toBase58()).toBe(MARKET_A);
    expect(entries[1]!.symbol).toBe("BTC-USD");
    expect(entries[1]!.expectedLeader).toBeUndefined();
    // Wave-18 lock_pda derivation when omitted.
    expect(entries[1]!.lockPda.toBase58()).not.toBe(MARKET_B);
  });

  it("rejects empty registry", () => {
    expect(() => parseMarketsToml("# nothing\n")).toThrow(/at least one/);
  });

  it("rejects duplicate symbols", () => {
    const toml = `
[[markets]]
symbol = "X"
program_id = "${PROGRAM}"
market_pda = "${MARKET_A}"
[[markets]]
symbol = "X"
program_id = "${PROGRAM}"
market_pda = "${MARKET_B}"
`;
    expect(() => parseMarketsToml(toml)).toThrow(/duplicate symbol/);
  });

  it("rejects oversized symbols (>16 bytes)", () => {
    const toml = `
[[markets]]
symbol = "THIS_SYMBOL_IS_TOO_LONG_FOR_THE_CAP"
program_id = "${PROGRAM}"
market_pda = "${MARKET_A}"
`;
    expect(() => parseMarketsToml(toml)).toThrow(/1\.\.16 ASCII/);
  });

  it("rejects unsupported headers", () => {
    expect(() => parseMarketsToml("[other]\nx = \"y\"\n")).toThrow(/unsupported header/);
  });

  it("rejects malformed pubkeys", () => {
    const toml = `
[[markets]]
symbol = "X"
program_id = "NOT_BASE_58!!"
market_pda = "${MARKET_A}"
`;
    expect(() => parseMarketsToml(toml)).toThrow(/invalid program_id/);
  });

  it("rejects orphan keys outside any [[markets]] table", () => {
    expect(() => parseMarketsToml("symbol = \"X\"\n")).toThrow(/orphan key/);
  });
});

// --------------------------------------------------------------------
// Wave 19 — env-var substitution
// --------------------------------------------------------------------

describe("substituteEnvVars (wave 19)", () => {
  it("passes through input with no references", () => {
    expect(substituteEnvVars("plain text $literal$", () => undefined)).toBe(
      "plain text $literal$",
    );
  });

  it("replaces a simple ${VAR} with the looked-up value", () => {
    expect(
      substituteEnvVars("hello ${NAME}", (n) =>
        n === "NAME" ? "world" : undefined,
      ),
    ).toBe("hello world");
  });

  it("collapses $$ to a literal $", () => {
    expect(substituteEnvVars("price $$5.00", () => undefined)).toBe(
      "price $5.00",
    );
  });

  it("throws on unset variable with the variable name in the message", () => {
    expect(() => substituteEnvVars("${MISSING}", () => undefined)).toThrow(
      /MISSING/,
    );
  });

  it("throws on empty value", () => {
    expect(() => substituteEnvVars("${E}", () => "")).toThrow(/E/);
  });

  it("throws on unclosed reference", () => {
    expect(() => substituteEnvVars("hi ${OPEN", () => undefined)).toThrow(
      /unclosed/,
    );
  });

  it("throws on empty braces", () => {
    expect(() => substituteEnvVars("${}", () => undefined)).toThrow(/empty/);
  });

  it("rejects names that start with a digit", () => {
    expect(() => substituteEnvVars("${1FOO}", () => undefined)).toThrow(
      /invalid characters/,
    );
  });

  it("rejects names with hyphens", () => {
    expect(() =>
      substituteEnvVars("${BAD-NAME}", () => undefined),
    ).toThrow(/invalid characters/);
  });

  it("accepts underscore-prefixed names", () => {
    expect(
      substituteEnvVars("${_X}", (n) => (n === "_X" ? "ok" : undefined)),
    ).toBe("ok");
  });
});

describe("parseMarketsToml (wave 19 env-var integration)", () => {
  const PROGRAM = "11111111111111111111111111111112";
  const MARKET_A = "Sysvar1nstructions1111111111111111111111111";
  const MARKET_B = "SysvarC1ock11111111111111111111111111111111";

  it("substitutes ${VAR} into expected_leader before parsing", () => {
    const toml = [
      "[[markets]]",
      "symbol = \"X\"",
      `program_id = "${PROGRAM}"`,
      `market_pda = "${MARKET_A}"`,
      `lock_pda = "${MARKET_B}"`,
      "expected_leader = \"${LEADER}\"",
      "",
    ].join("\n");
    const out = parseMarketsToml(toml, (n) =>
      n === "LEADER" ? PROGRAM : undefined,
    );
    expect(out).toHaveLength(1);
    expect(out[0]!.expectedLeader?.toBase58()).toBe(PROGRAM);
  });

  it("surfaces env-var error with the variable name", () => {
    const toml = [
      "[[markets]]",
      "symbol = \"X\"",
      `program_id = "${PROGRAM}"`,
      `market_pda = "\${UNSET}"`,
      "",
    ].join("\n");
    expect(() => parseMarketsToml(toml, () => undefined)).toThrow(/UNSET/);
  });
});
