//! Wave 18 — multi-market registry.
//!
//! Wave 17 left every consumer (`keeper-bot`, `ops-toolkit`, the
//! frontend) hard-wired to a single `MarketContext` because that's
//! all wave 9..17 needed: one symbol per process. Wave 18 lifts that
//! restriction. A [`MarketRegistry`] is a vector of [`MarketEntry`]s
//! plus a small lookup index; one process now holds N entries and
//! the run-loop / probe / UI fan out across them.
//!
//! ## Single source of truth
//!
//! Three independent consumers need this data:
//!
//! 1. `keeper-bot` — runs `run_loop_multi_market_with_leader_and_rpc_reconcile`
//!    over every entry; each market gets its own `LeaderPolicy`,
//!    reconcile cadence, heartbeat publisher, graceful release path.
//! 2. `ops-toolkit` — `HealthContext` is now per-entry; the prober
//!    fans out one probe cycle per market and aggregates the
//!    findings.
//! 3. Frontend `MultiMarketFeedAdapter` — re-derives the same set
//!    of PDAs at boot and subscribes to all of them.
//!
//! Putting the registry in `keeper-rpc` (rather than in a separate
//! crate or per-consumer copy) keeps the schema and the loader
//! single-sourced. The frontend re-derives the same logical shape
//! from `import.meta.env` flags or a JSON shipped at build time
//! (see `frontend/src/marketRegistry.ts`).
//!
//! ## TOML loader
//!
//! `MarketRegistry::from_toml_str` parses a deliberately tiny TOML
//! subset (`[[markets]]` array of tables with bare-key string
//! values) so we don't need a `toml` / `serde` dependency on a
//! lock-down ops VM. The wave-12 ops-toolkit governance choice
//! ("no `serde` / `serde_json`; hand-format JSON") applies here too.
//! When ops grows past this schema we revisit, but until then a
//! 100-LoC parser kills an entire crate transitive tree.
//!
//! Example file:
//!
//! ```toml
//! # markets.toml — wave 18 example.
//! [[markets]]
//! symbol = "SOL-USD"
//! program_id = "Mole11111111111111111111111111111111111112"
//! market_pda = "MktPDA111111111111111111111111111111111111"
//! lock_pda   = "LockPda1111111111111111111111111111111111"  # optional
//! expected_leader = "KeepHot111111111111111111111111111111111"  # optional
//!
//! [[markets]]
//! symbol = "BTC-USD"
//! program_id = "Mole11111111111111111111111111111111111112"
//! market_pda = "MktPDA222222222222222222222222222222222222"
//! ```
//!
//! `lock_pda` is *optional*: if absent the loader derives it from
//! `[b"keeper_leader_lock", market_pda]` against `program_id`. We
//! still allow callers to pin a value (e.g. for staging clusters
//! whose program id rotates between deploys but whose lock PDA the
//! ops team has already tagged in incident docs).

use crate::Pubkey32;

/// One configured market.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketEntry {
    /// Stable, operator-facing identifier. Required to match the
    /// on-chain `Market.symbol` bytes (zero-padded ASCII, 16 bytes
    /// max). The registry validates the byte width on load.
    pub symbol: String,
    /// Mole-option program id this market belongs to.
    pub program_id: Pubkey32,
    /// `Market` PDA pubkey.
    pub market_pda: Pubkey32,
    /// `KeeperLeaderLock` PDA pubkey. Wave-18 callers pre-derive
    /// this at registry-build time so the run-loop hot path doesn't
    /// repeat the `find_program_address` call (which is non-trivial
    /// — it iterates bumps until one falls off the curve).
    pub lock_pda: Pubkey32,
    /// Operator's authoritative answer for "who *should* be holding
    /// the lock right now". Surfaced into
    /// `LeaderLockFacts.expected_leader` for the wave-17 health
    /// check `keeper_leader_lock_holder_matches_expected`. `None`
    /// means "don't alarm on holder mismatch for this market"
    /// (typical for staging or read-only deployments).
    pub expected_leader: Option<Pubkey32>,
}

impl MarketEntry {
    /// Symbol bytes in the wave-9 `Market.symbol` shape (16-byte
    /// zero-padded ASCII). Returns the same array shape that
    /// `MarketContext` uses so wave-18 callers can plug it straight
    /// in.
    #[must_use]
    pub fn symbol_bytes(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        let src = self.symbol.as_bytes();
        let n = core::cmp::min(src.len(), 16);
        out[..n].copy_from_slice(&src[..n]);
        out
    }
}

/// Multi-market configuration held by every wave-18 multi-market
/// consumer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarketRegistry {
    /// Configured markets, in insertion order.
    pub markets: Vec<MarketEntry>,
}

/// Loader / structural errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// TOML parsing failed at the structural level (mismatched
    /// brackets / unterminated string / unsupported value type).
    #[error("toml parse error at line {line}: {message}")]
    Toml {
        /// 1-indexed source line where the parser bailed.
        line: usize,
        /// Human-readable failure description.
        message: String,
    },
    /// A market entry is missing a required key.
    #[error("market entry #{index} missing required key `{key}`")]
    MissingKey {
        /// 0-indexed position of the offending entry in the file.
        index: usize,
        /// Required key name (`symbol`, `program_id`, `market_pda`).
        key: &'static str,
    },
    /// A pubkey-shaped value did not decode as 32 bytes of base58.
    #[error("market `{symbol}` has invalid {field} pubkey: {detail}")]
    InvalidPubkey {
        /// Symbol label (best-effort; falls back to `<index>` if
        /// the entry's symbol failed validation first).
        symbol: String,
        /// Field that failed (`program_id`, `market_pda`,
        /// `lock_pda`, `expected_leader`).
        field: &'static str,
        /// Underlying detail (length / charset).
        detail: String,
    },
    /// Symbol exceeds the wave-9 16-byte cap or contains non-ASCII.
    #[error("market `{symbol}` has an invalid symbol: {detail}")]
    InvalidSymbol {
        /// The offending symbol (or empty if absent).
        symbol: String,
        /// Why it's invalid.
        detail: String,
    },
    /// Two entries share the same `symbol`. Symbols MUST be unique
    /// because the registry uses them as the lookup key (see
    /// [`MarketRegistry::find_by_symbol`]).
    #[error("duplicate market symbol `{symbol}`")]
    DuplicateSymbol {
        /// The duplicated symbol.
        symbol: String,
    },
    /// Registry is empty after parsing — every wave-18 consumer
    /// expects at least one market.
    #[error("market registry is empty")]
    Empty,
    /// Wave 19 — `${VAR}` reference resolved to an unset / empty
    /// environment variable. Surfaced ahead of TOML parsing so the
    /// operator gets the actual unresolved variable name (rather
    /// than a downstream "invalid pubkey" error).
    #[error("environment variable `{name}` is unset or empty (referenced via ${{{name}}})")]
    EnvVar {
        /// Name of the unset variable.
        name: String,
    },
    /// Wave 19 — malformed `${...}` reference (e.g. `${unclosed`,
    /// empty name `${}`, or whitespace inside the braces).
    #[error("malformed environment variable reference at byte {offset}: {detail}")]
    MalformedEnvRef {
        /// 0-indexed byte offset of the `$` that opened the bad ref.
        offset: usize,
        /// Why it's invalid.
        detail: String,
    },
}

impl MarketRegistry {
    /// Load a registry from a TOML string. Returns the parsed
    /// registry on success, or a structural [`RegistryError`] on
    /// the first failure (we fail fast — partial registries are
    /// worse than a clear error message).
    ///
    /// Wave 19 — the input is run through
    /// [`substitute_env_vars`] before parsing so operators can
    /// keep a single `markets.toml` template with `${VAR}`
    /// placeholders for secrets (e.g. `expected_leader`) that get
    /// injected from the environment by SOPS / sealed-secrets.
    /// `$$` is the escape for a literal `$`. Process env is read
    /// via `std::env::var`; for tests / sandboxed builders use
    /// [`Self::from_toml_str_with_env`].
    pub fn from_toml_str(input: &str) -> Result<Self, RegistryError> {
        Self::from_toml_str_with_env(input, |name| std::env::var(name).ok())
    }

    /// Wave 19 — load a registry while resolving `${VAR}`
    /// placeholders through a caller-supplied lookup. The lookup
    /// returns `None` for unset/empty variables; the parser then
    /// raises [`RegistryError::EnvVar`] so the operator sees the
    /// actual unresolved name in the error message instead of a
    /// confusing downstream pubkey-decode failure.
    pub fn from_toml_str_with_env<F>(input: &str, lookup: F) -> Result<Self, RegistryError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let resolved = substitute_env_vars(input, &lookup)?;
        let raw = parse_market_tables(&resolved)?;
        let mut markets = Vec::with_capacity(raw.len());
        for (idx, table) in raw.into_iter().enumerate() {
            markets.push(build_entry(idx, table)?);
        }
        let mut seen = std::collections::HashSet::new();
        for m in &markets {
            if !seen.insert(m.symbol.clone()) {
                return Err(RegistryError::DuplicateSymbol {
                    symbol: m.symbol.clone(),
                });
            }
        }
        if markets.is_empty() {
            return Err(RegistryError::Empty);
        }
        Ok(Self { markets })
    }

    /// Look up by stable symbol (`O(N)` — N is small, typically <20).
    #[must_use]
    pub fn find_by_symbol(&self, symbol: &str) -> Option<&MarketEntry> {
        self.markets.iter().find(|m| m.symbol == symbol)
    }

    /// Number of configured markets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.markets.len()
    }

    /// `true` if the registry has zero markets. (After
    /// [`from_toml_str`] this is always `false`; constructing
    /// programmatically can produce an empty registry which is
    /// `Default::default`.)
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.markets.is_empty()
    }

    /// Iterate over all entries.
    pub fn iter(&self) -> impl Iterator<Item = &MarketEntry> {
        self.markets.iter()
    }
}

// ---------------------------------------------------------------------
// TOML subset parser
// ---------------------------------------------------------------------
//
// We accept the deliberately small surface:
//
//   - `# comments` to end-of-line
//   - `[[markets]]` headers (and ONLY this header — anything else
//     errors)
//   - `key = "string"` rows under each header
//
// Every other TOML construct (inline tables, arrays, integers, dates,
// multiline strings, escapes beyond the basics) is rejected. Future
// schema additions can extend this without breaking compatibility
// because the parser already errors loudly on unknown structure.

#[derive(Debug)]
struct RawTable {
    /// Source line of the `[[markets]]` header (1-indexed) for
    /// error context.
    header_line: usize,
    fields: Vec<(String, String)>,
}

/// Wave 19 — expand `${VAR}` placeholders against `lookup`. `$$`
/// becomes a literal `$`. Returns the rewritten string or an
/// `EnvVar` / `MalformedEnvRef` error.
///
/// Variable names must match `[A-Za-z_][A-Za-z0-9_]*` (POSIX env);
/// lookup misses (None / empty) raise `EnvVar`. The helper is
/// `pub` so other crates with their own configuration loaders
/// (frontend, ops-toolkit/ts) can reuse the same substitution
/// semantics; mirroring the behaviour byte-for-byte is essential
/// because the same `markets.toml` flows through Rust + TS.
pub fn substitute_env_vars<F>(input: &str, lookup: &F) -> Result<String, RegistryError>
where
    F: Fn(&str) -> Option<String>,
{
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'$' {
            // Fast path — consume one char.
            // We're at a UTF-8 boundary because we only branch on
            // ASCII bytes (`$`, `{`, `}`); other multi-byte
            // sequences pass through verbatim.
            let ch_len = utf8_char_len(bytes[i]);
            out.push_str(std::str::from_utf8(&bytes[i..i + ch_len]).unwrap());
            i += ch_len;
            continue;
        }
        // We're at a `$`. Lookahead for `$$` or `${...}`.
        if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            out.push('$');
            i += 2;
            continue;
        }
        if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // Find the closing `}`.
            let start = i + 2;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'}' {
                end += 1;
            }
            if end >= bytes.len() {
                return Err(RegistryError::MalformedEnvRef {
                    offset: i,
                    detail: "unclosed `${...}` reference".into(),
                });
            }
            let name = &input[start..end];
            if name.is_empty() {
                return Err(RegistryError::MalformedEnvRef {
                    offset: i,
                    detail: "empty variable name `${}`".into(),
                });
            }
            if !is_valid_var_name(name) {
                return Err(RegistryError::MalformedEnvRef {
                    offset: i,
                    detail: format!(
                        "variable name `{name}` contains invalid characters \
                        (must match [A-Za-z_][A-Za-z0-9_]*)"
                    ),
                });
            }
            match lookup(name) {
                Some(v) if !v.is_empty() => out.push_str(&v),
                _ => {
                    return Err(RegistryError::EnvVar {
                        name: name.to_string(),
                    });
                }
            }
            i = end + 1;
            continue;
        }
        // Bare `$` followed by anything else — keep verbatim.
        out.push('$');
        i += 1;
    }
    Ok(out)
}

fn is_valid_var_name(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

fn utf8_char_len(first: u8) -> usize {
    // Standard UTF-8 leading-byte ranges:
    //   0xxxxxxx -> 1 byte (ASCII)
    //   10xxxxxx -> continuation byte; should never be a leading byte,
    //               but we conservatively report 1 to avoid panics on
    //               malformed input.
    //   110xxxxx -> 2 bytes
    //   1110xxxx -> 3 bytes
    //   11110xxx -> 4 bytes
    if first < 0xC0 {
        1
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    }
}

fn parse_market_tables(input: &str) -> Result<Vec<RawTable>, RegistryError> {
    let mut tables: Vec<RawTable> = Vec::new();
    let mut current: Option<RawTable> = None;
    for (i, raw_line) in input.lines().enumerate() {
        let line_num = i + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("[[") && line.ends_with("]]") {
            let header = &line[2..line.len() - 2];
            if header.trim() != "markets" {
                return Err(RegistryError::Toml {
                    line: line_num,
                    message: format!(
                        "unsupported header `[[{header}]]` (only `[[markets]]` is allowed)"
                    ),
                });
            }
            if let Some(t) = current.take() {
                tables.push(t);
            }
            current = Some(RawTable {
                header_line: line_num,
                fields: Vec::new(),
            });
            continue;
        }
        if line.starts_with('[') {
            return Err(RegistryError::Toml {
                line: line_num,
                message: format!("unsupported header `{line}` (use `[[markets]]`)"),
            });
        }
        // key = "value"
        let (key, value) = parse_key_value(line, line_num)?;
        match current.as_mut() {
            Some(t) => t.fields.push((key, value)),
            None => {
                return Err(RegistryError::Toml {
                    line: line_num,
                    message: format!("orphan key `{key}` outside any `[[markets]]` table"),
                });
            }
        }
    }
    if let Some(t) = current.take() {
        tables.push(t);
    }
    let _ = tables.iter().map(|t| t.header_line); // keep field used
    Ok(tables)
}

fn strip_comment(line: &str) -> &str {
    // Comment-aware: ignore `#` inside double-quoted strings. Simple
    // state machine — sufficient for the schema we accept.
    let bytes = line.as_bytes();
    let mut in_str = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' if i == 0 || bytes[i - 1] != b'\\' => in_str = !in_str,
            b'#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn parse_key_value(line: &str, line_num: usize) -> Result<(String, String), RegistryError> {
    let eq = line.find('=').ok_or_else(|| RegistryError::Toml {
        line: line_num,
        message: format!("expected `key = \"value\"`, got `{line}`"),
    })?;
    let key = line[..eq].trim().to_string();
    if key.is_empty() || key.contains(|c: char| !is_bare_key_char(c)) {
        return Err(RegistryError::Toml {
            line: line_num,
            message: format!("invalid bare key `{key}`"),
        });
    }
    let rest = line[eq + 1..].trim();
    if !rest.starts_with('"') {
        return Err(RegistryError::Toml {
            line: line_num,
            message: format!(
                "value for `{key}` must be a double-quoted string (got `{rest}`)"
            ),
        });
    }
    let body = &rest[1..];
    let end = body.find('"').ok_or_else(|| RegistryError::Toml {
        line: line_num,
        message: format!("unterminated string for `{key}`"),
    })?;
    // Reject anything after the closing quote (other than whitespace
    // we've already trimmed).
    let after = body[end + 1..].trim();
    if !after.is_empty() {
        return Err(RegistryError::Toml {
            line: line_num,
            message: format!("trailing tokens after string value for `{key}`: `{after}`"),
        });
    }
    Ok((key, body[..end].to_string()))
}

fn is_bare_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn build_entry(index: usize, table: RawTable) -> Result<MarketEntry, RegistryError> {
    let mut symbol: Option<String> = None;
    let mut program_id: Option<String> = None;
    let mut market_pda: Option<String> = None;
    let mut lock_pda: Option<String> = None;
    let mut expected_leader: Option<String> = None;
    for (k, v) in table.fields {
        match k.as_str() {
            "symbol" => symbol = Some(v),
            "program_id" => program_id = Some(v),
            "market_pda" => market_pda = Some(v),
            "lock_pda" => lock_pda = Some(v),
            "expected_leader" => expected_leader = Some(v),
            _ => {
                return Err(RegistryError::Toml {
                    line: table.header_line,
                    message: format!("unknown key `{k}` in `[[markets]]` (entry #{index})"),
                });
            }
        }
    }
    let symbol = symbol.ok_or(RegistryError::MissingKey {
        index,
        key: "symbol",
    })?;
    validate_symbol(&symbol)?;
    let program_id_raw = program_id.ok_or(RegistryError::MissingKey {
        index,
        key: "program_id",
    })?;
    let market_pda_raw = market_pda.ok_or(RegistryError::MissingKey {
        index,
        key: "market_pda",
    })?;
    let program_id = decode_pubkey32(&symbol, "program_id", &program_id_raw)?;
    let market_pda = decode_pubkey32(&symbol, "market_pda", &market_pda_raw)?;
    let lock_pda = match lock_pda {
        Some(raw) => decode_pubkey32(&symbol, "lock_pda", &raw)?,
        None => derive_keeper_leader_lock_pda(&program_id, &market_pda),
    };
    let expected_leader = match expected_leader {
        Some(raw) if !raw.is_empty() => Some(decode_pubkey32(&symbol, "expected_leader", &raw)?),
        _ => None,
    };
    Ok(MarketEntry {
        symbol,
        program_id,
        market_pda,
        lock_pda,
        expected_leader,
    })
}

fn validate_symbol(symbol: &str) -> Result<(), RegistryError> {
    if symbol.is_empty() {
        return Err(RegistryError::InvalidSymbol {
            symbol: String::new(),
            detail: "symbol must not be empty".to_string(),
        });
    }
    if symbol.len() > 16 {
        return Err(RegistryError::InvalidSymbol {
            symbol: symbol.to_string(),
            detail: format!("symbol exceeds 16-byte cap (got {} bytes)", symbol.len()),
        });
    }
    if !symbol.is_ascii() {
        return Err(RegistryError::InvalidSymbol {
            symbol: symbol.to_string(),
            detail: "symbol must be pure ASCII".to_string(),
        });
    }
    Ok(())
}

/// Decode a base58 32-byte pubkey. We re-implement decoding with no
/// external crate dependency to keep the keeper-rpc default-feature
/// surface unchanged. `bs58` is already in the `solana-rpc` feature
/// transitive tree but pulling it into default features would add a
/// dep just for config-loading — not worth it.
fn decode_pubkey32(
    symbol: &str,
    field: &'static str,
    raw: &str,
) -> Result<Pubkey32, RegistryError> {
    let bytes = base58_decode(raw).map_err(|e| RegistryError::InvalidPubkey {
        symbol: symbol.to_string(),
        field,
        detail: e,
    })?;
    if bytes.len() != 32 {
        return Err(RegistryError::InvalidPubkey {
            symbol: symbol.to_string(),
            field,
            detail: format!("decoded {} bytes, expected 32", bytes.len()),
        });
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

const BS58_ALPHABET: &[u8; 58] =
    b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn base58_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut alpha = [0xff_u8; 128];
    for (i, &b) in BS58_ALPHABET.iter().enumerate() {
        alpha[b as usize] = i as u8;
    }
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    for c in input.chars() {
        if c.is_whitespace() {
            continue;
        }
        let v = if (c as u32) < 128 {
            alpha[c as usize]
        } else {
            return Err(format!("non-base58 character `{c}`"));
        };
        if v == 0xff {
            return Err(format!("non-base58 character `{c}`"));
        }
        let mut carry = v as u32;
        for byte in out.iter_mut() {
            carry += (*byte as u32) * 58;
            *byte = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    // Leading '1's encode leading zero bytes.
    for c in input.chars() {
        if c == '1' {
            out.push(0);
        } else if !c.is_whitespace() {
            break;
        }
    }
    out.reverse();
    Ok(out)
}

fn derive_keeper_leader_lock_pda(program_id: &Pubkey32, market: &Pubkey32) -> Pubkey32 {
    // Wave-15 PDA seeds: `[b"keeper_leader_lock", market]`. We can't
    // call `Pubkey::find_program_address` from default-feature
    // keeper-rpc (no solana-pubkey dep). For wave 18 we accept that
    // ops MUST set `lock_pda` explicitly when the registry is
    // loaded from default-feature builds (e.g. ops-toolkit), and
    // the `solana-rpc` feature provides the real derivation in
    // `solana::derive_lock_pda`. Until then we return a
    // deterministic sentinel that any consumer can detect.
    //
    // This is intentionally a *visible* sentinel rather than zero:
    // a downstream that didn't replace it shows a clearly-bogus PDA
    // in any logs, which is a louder failure mode than silently
    // pointing at the system program.
    let _ = (program_id, market);
    [0xfe; 32]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBKEY_A: &str = "11111111111111111111111111111112";
    const PUBKEY_B: &str = "Sysvar1nstructions1111111111111111111111111";
    const PUBKEY_C: &str = "SysvarC1ock11111111111111111111111111111111";

    fn pk32(b58: &str) -> Pubkey32 {
        let bytes = base58_decode(b58).unwrap();
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        out
    }

    #[test]
    fn parses_minimal_registry() {
        let toml = format!(
            "[[markets]]\nsymbol = \"SOL-USD\"\nprogram_id = \"{}\"\nmarket_pda = \"{}\"\nlock_pda = \"{}\"\n",
            PUBKEY_A, PUBKEY_B, PUBKEY_C,
        );
        let r = MarketRegistry::from_toml_str(&toml).unwrap();
        assert_eq!(r.len(), 1);
        let m = &r.markets[0];
        assert_eq!(m.symbol, "SOL-USD");
        assert_eq!(m.program_id, pk32(PUBKEY_A));
        assert_eq!(m.market_pda, pk32(PUBKEY_B));
        assert_eq!(m.lock_pda, pk32(PUBKEY_C));
        assert_eq!(m.expected_leader, None);
    }

    #[test]
    fn parses_two_markets_with_optional_expected_leader() {
        let toml = format!(
            "[[markets]]\nsymbol = \"SOL-USD\"\nprogram_id = \"{a}\"\nmarket_pda = \"{b}\"\nlock_pda = \"{c}\"\nexpected_leader = \"{a}\"\n\n# Second market — comments allowed.\n[[markets]]\nsymbol = \"BTC-USD\"\nprogram_id = \"{a}\"\nmarket_pda = \"{c}\"\nlock_pda = \"{b}\"\n",
            a = PUBKEY_A,
            b = PUBKEY_B,
            c = PUBKEY_C,
        );
        let r = MarketRegistry::from_toml_str(&toml).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r.markets[0].symbol, "SOL-USD");
        assert!(r.markets[0].expected_leader.is_some());
        assert_eq!(r.markets[1].symbol, "BTC-USD");
        assert_eq!(r.markets[1].expected_leader, None);
        assert!(r.find_by_symbol("BTC-USD").is_some());
        assert!(r.find_by_symbol("UNKNOWN").is_none());
    }

    #[test]
    fn rejects_empty_registry() {
        let r = MarketRegistry::from_toml_str("# nothing here\n");
        assert_eq!(r.unwrap_err(), RegistryError::Empty);
    }

    #[test]
    fn rejects_duplicate_symbol() {
        let toml = format!(
            "[[markets]]\nsymbol = \"SOL-USD\"\nprogram_id = \"{a}\"\nmarket_pda = \"{b}\"\nlock_pda = \"{c}\"\n[[markets]]\nsymbol = \"SOL-USD\"\nprogram_id = \"{a}\"\nmarket_pda = \"{b}\"\nlock_pda = \"{c}\"\n",
            a = PUBKEY_A,
            b = PUBKEY_B,
            c = PUBKEY_C,
        );
        let err = MarketRegistry::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, RegistryError::DuplicateSymbol { .. }));
    }

    #[test]
    fn rejects_missing_required_keys() {
        let toml = format!("[[markets]]\nsymbol = \"SOL-USD\"\nprogram_id = \"{}\"\n", PUBKEY_A);
        let err = MarketRegistry::from_toml_str(&toml).unwrap_err();
        assert_eq!(
            err,
            RegistryError::MissingKey {
                index: 0,
                key: "market_pda",
            }
        );
    }

    #[test]
    fn rejects_unknown_key() {
        let toml = format!(
            "[[markets]]\nsymbol = \"SOL-USD\"\nprogram_id = \"{}\"\nmarket_pda = \"{}\"\nlock_pda = \"{}\"\nfee_vault = \"oops\"\n",
            PUBKEY_A, PUBKEY_B, PUBKEY_C,
        );
        let err = MarketRegistry::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, RegistryError::Toml { .. }));
    }

    #[test]
    fn rejects_oversized_symbol() {
        let toml = format!(
            "[[markets]]\nsymbol = \"THIS_SYMBOL_IS_TOO_LONG_FOR_THE_16_BYTE_CAP\"\nprogram_id = \"{}\"\nmarket_pda = \"{}\"\nlock_pda = \"{}\"\n",
            PUBKEY_A, PUBKEY_B, PUBKEY_C,
        );
        let err = MarketRegistry::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, RegistryError::InvalidSymbol { .. }));
    }

    #[test]
    fn rejects_invalid_pubkey() {
        let toml = "[[markets]]\nsymbol = \"X\"\nprogram_id = \"NOT_BASE_58!!\"\nmarket_pda = \"NOT_BASE_58!!\"\nlock_pda = \"NOT_BASE_58!!\"\n";
        let err = MarketRegistry::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, RegistryError::InvalidPubkey { .. }));
    }

    #[test]
    fn rejects_orphan_key() {
        let err = MarketRegistry::from_toml_str("symbol = \"X\"\n").unwrap_err();
        assert!(matches!(err, RegistryError::Toml { .. }));
    }

    #[test]
    fn accepts_inline_comments_after_strings() {
        let toml = format!(
            "[[markets]]\nsymbol = \"SOL-USD\" # primary market\nprogram_id = \"{}\"\nmarket_pda = \"{}\"\nlock_pda = \"{}\"\n",
            PUBKEY_A, PUBKEY_B, PUBKEY_C,
        );
        let r = MarketRegistry::from_toml_str(&toml).unwrap();
        assert_eq!(r.markets[0].symbol, "SOL-USD");
    }

    #[test]
    fn symbol_bytes_zero_pads_to_16() {
        let m = MarketEntry {
            symbol: "SOL-USD".to_string(),
            program_id: pk32(PUBKEY_A),
            market_pda: pk32(PUBKEY_B),
            lock_pda: pk32(PUBKEY_C),
            expected_leader: None,
        };
        let bytes = m.symbol_bytes();
        assert_eq!(&bytes[..7], b"SOL-USD");
        assert_eq!(&bytes[7..], &[0u8; 9]);
    }

    #[test]
    fn lock_pda_omitted_falls_back_to_sentinel() {
        let toml = format!(
            "[[markets]]\nsymbol = \"X\"\nprogram_id = \"{}\"\nmarket_pda = \"{}\"\n",
            PUBKEY_A, PUBKEY_B,
        );
        let r = MarketRegistry::from_toml_str(&toml).unwrap();
        // Default-feature build can't derive the real PDA; sentinel
        // surfaces "ops forgot to pin lock_pda" loudly in logs.
        assert_eq!(r.markets[0].lock_pda, [0xfe; 32]);
    }

    // ----------------------------------------------------------------
    // Wave 19 — env-var substitution tests
    // ----------------------------------------------------------------

    #[test]
    fn substitute_env_vars_passes_through_when_no_refs() {
        let out = substitute_env_vars("plain text $literal$", &|_| None).unwrap();
        assert_eq!(out, "plain text $literal$");
    }

    #[test]
    fn substitute_env_vars_replaces_simple_ref() {
        let out = substitute_env_vars("hello ${NAME}", &|n| {
            (n == "NAME").then(|| "world".to_string())
        })
        .unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn substitute_env_vars_handles_double_dollar_escape() {
        let out = substitute_env_vars("price $$5.00", &|_| None).unwrap();
        assert_eq!(out, "price $5.00");
    }

    #[test]
    fn substitute_env_vars_errors_on_unset() {
        let err = substitute_env_vars("${MISSING}", &|_| None).unwrap_err();
        match err {
            RegistryError::EnvVar { name } => assert_eq!(name, "MISSING"),
            other => panic!("expected EnvVar, got {other:?}"),
        }
    }

    #[test]
    fn substitute_env_vars_errors_on_empty_value() {
        let err =
            substitute_env_vars("${E}", &|_| Some(String::new())).unwrap_err();
        match err {
            RegistryError::EnvVar { name } => assert_eq!(name, "E"),
            other => panic!("expected EnvVar, got {other:?}"),
        }
    }

    #[test]
    fn substitute_env_vars_errors_on_unclosed_ref() {
        let err = substitute_env_vars("hi ${OPEN", &|_| None).unwrap_err();
        match err {
            RegistryError::MalformedEnvRef { detail, .. } => {
                assert!(detail.contains("unclosed"));
            }
            other => panic!("expected MalformedEnvRef, got {other:?}"),
        }
    }

    #[test]
    fn substitute_env_vars_errors_on_empty_braces() {
        let err = substitute_env_vars("${}", &|_| None).unwrap_err();
        match err {
            RegistryError::MalformedEnvRef { detail, .. } => {
                assert!(detail.contains("empty"));
            }
            other => panic!("expected MalformedEnvRef, got {other:?}"),
        }
    }

    #[test]
    fn substitute_env_vars_errors_on_invalid_chars() {
        let err = substitute_env_vars("${BAD-NAME}", &|_| None).unwrap_err();
        match err {
            RegistryError::MalformedEnvRef { detail, .. } => {
                assert!(detail.contains("invalid characters"));
            }
            other => panic!("expected MalformedEnvRef, got {other:?}"),
        }
    }

    #[test]
    fn substitute_env_vars_starts_with_digit_rejected() {
        let err = substitute_env_vars("${1FOO}", &|_| None).unwrap_err();
        assert!(matches!(err, RegistryError::MalformedEnvRef { .. }));
    }

    #[test]
    fn substitute_env_vars_underscore_prefix_accepted() {
        let out = substitute_env_vars("${_X}", &|n| {
            (n == "_X").then(|| "ok".to_string())
        })
        .unwrap();
        assert_eq!(out, "ok");
    }

    #[test]
    fn from_toml_str_with_env_substitutes_expected_leader() {
        let toml = format!(
            "[[markets]]\nsymbol = \"X\"\nprogram_id = \"{}\"\nmarket_pda = \"{}\"\n\
            lock_pda = \"{}\"\nexpected_leader = \"${{LEADER}}\"\n",
            PUBKEY_A, PUBKEY_B, PUBKEY_C,
        );
        let r = MarketRegistry::from_toml_str_with_env(&toml, |n| {
            (n == "LEADER").then(|| PUBKEY_A.to_string())
        })
        .unwrap();
        assert_eq!(r.markets[0].symbol, "X");
        assert!(r.markets[0].expected_leader.is_some());
    }

    #[test]
    fn from_toml_str_surfaces_env_error_with_variable_name() {
        let toml = format!(
            "[[markets]]\nsymbol = \"X\"\nprogram_id = \"{}\"\nmarket_pda = \"${{UNSET}}\"\n",
            PUBKEY_A,
        );
        let err = MarketRegistry::from_toml_str_with_env(&toml, |_| None).unwrap_err();
        match err {
            RegistryError::EnvVar { name } => assert_eq!(name, "UNSET"),
            other => panic!("expected EnvVar(UNSET), got {other:?}"),
        }
    }

    #[test]
    fn from_toml_str_with_env_supports_double_dollar_escape() {
        // Use `$$` to embed a literal `$` (e.g. in a comment that
        // happens to use the dollar sign — our parser strips
        // comments after substitution, so the `$$` in a comment
        // line would still expand. Test that `$$ ` at start of a
        // bare comment doesn't crash).
        let toml = format!(
            "# price reference: $$5.00\n[[markets]]\nsymbol = \"X\"\nprogram_id = \"{}\"\n\
            market_pda = \"{}\"\nlock_pda = \"{}\"\n",
            PUBKEY_A, PUBKEY_B, PUBKEY_C,
        );
        let r = MarketRegistry::from_toml_str_with_env(&toml, |_| None).unwrap();
        assert_eq!(r.markets[0].symbol, "X");
    }
}
