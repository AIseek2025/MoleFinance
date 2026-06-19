//! Wave 20 — CLI helpers for loading `markets.toml` and an
//! optional environment overlay through stdin / files. These are
//! host-testable pure functions so the SOPS-pipe behaviour (the
//! whole reason this module exists) is verified without ever
//! touching the filesystem.
//!
//! The wave-19 prober binary loads `markets.toml` from a file
//! path and reads `${VAR}` substitutions from `std::env`. That
//! works, but two production needs surface:
//!
//!   1. **No plaintext on disk.** Operators want to pipe a SOPS-
//!      decrypted `markets.toml` directly into the prober without
//!      ever materialising a temp file (`sops -d markets.enc.toml
//!      | ops-toolkit prober --markets-stdin ...`).
//!   2. **No environ leakage.** Same reason — `${VAR}` references
//!      should be resolved against an *encrypted-at-rest* env
//!      file, not the long-lived shell environment that may have
//!      been logged by parent processes.
//!
//! This module ships:
//!
//!   - `MarketsSource` — `File(path)` or `Stdin`.
//!   - `EnvSource` — `Process` (default), `File(path)`, or
//!     `Inline(HashMap)` (test path).
//!   - `load_registry` — loads, applies env overlay, parses TOML.
//!
//! Argument parsing follows the wave-19 minimal positional style
//! plus optional `--markets-stdin` / `--env-from-file=PATH`
//! flags. Wave 20 deliberately keeps `clap` out of the dep tree
//! to preserve the wave-12 "no new deps in default features"
//! invariant.

use std::collections::HashMap;
use std::io::Read;

use crate::multi::MarketRegistry;

/// Where the `markets.toml` content comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketsSource {
    /// Read from a path on disk.
    File(String),
    /// Read from process stdin until EOF.
    Stdin,
}

/// Where `${VAR}` substitution values come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvSource {
    /// Default — the live process environment via
    /// `std::env::var`.
    Process,
    /// A KEY=VALUE file, one per line. Comments (`#`) and blank
    /// lines are ignored. Missing keys *fall back* to the live
    /// process environment so callers can mix in long-lived
    /// non-secret values without splitting their config.
    File(String),
    /// Inline lookup table (test fixture). Same fallback
    /// semantics as `File`.
    Inline(HashMap<String, String>),
}

/// Wave 20 — parse a flat `KEY=VALUE` env file. Returns the
/// resulting map. Tolerates `export KEY=VALUE`, surrounding
/// double-quotes (stripped), and comments. Errors on malformed
/// lines that don't contain an `=`.
pub fn parse_env_file(text: &str) -> Result<HashMap<String, String>, String> {
    let mut out = HashMap::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `export KEY=VALUE` shorthand — strip the leading prefix.
        let line = line.strip_prefix("export ").unwrap_or(line);
        let eq = line
            .find('=')
            .ok_or_else(|| format!("env-file line {}: missing `=`", idx + 1))?;
        let key = line[..eq].trim();
        if key.is_empty() {
            return Err(format!("env-file line {}: empty key", idx + 1));
        }
        if !is_valid_env_var_name(key) {
            return Err(format!("env-file line {}: invalid key `{key}`", idx + 1));
        }
        let mut val = line[eq + 1..].trim().to_string();
        // Strip optional surrounding double-quotes — common shell
        // idiom for values containing spaces. Don't process
        // backslash escapes; that's not in scope for wave 20 and
        // would diverge from `${VAR}` byte-level semantics.
        if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
            val = val[1..val.len() - 1].to_string();
        }
        out.insert(key.to_string(), val);
    }
    Ok(out)
}

fn is_valid_env_var_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut bytes = name.bytes();
    let first = bytes.next().expect("len > 0");
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    for b in bytes {
        if !(b.is_ascii_alphanumeric() || b == b'_') {
            return false;
        }
    }
    true
}

/// Wave 20 — load a `MarketRegistry` from the configured sources.
/// Generic over a TOML reader / stdin reader / env reader so all
/// I/O is mockable.
///
/// `read_path`: callable returning the file contents for a path.
/// `read_stdin`: callable returning the stdin text (no path).
/// `read_env`: callable returning the value for a single env var
/// from the *process* environment — only invoked when the
/// resolved env source is `Process` or as a fallback for `File` /
/// `Inline`.
pub fn load_registry<F, S, P>(
    markets: &MarketsSource,
    env: &EnvSource,
    mut read_path: F,
    mut read_stdin: S,
    read_env: P,
) -> Result<MarketRegistry, String>
where
    F: FnMut(&str) -> Result<String, String>,
    S: FnMut() -> Result<String, String>,
    P: Fn(&str) -> Option<String>,
{
    let toml_text = match markets {
        MarketsSource::File(path) => read_path(path)?,
        MarketsSource::Stdin => read_stdin()?,
    };
    let env_table: HashMap<String, String> = match env {
        EnvSource::Process => HashMap::new(),
        EnvSource::File(path) => {
            let text = read_path(path)?;
            parse_env_file(&text)?
        }
        EnvSource::Inline(map) => map.clone(),
    };
    let lookup = |k: &str| -> Option<String> {
        if let Some(v) = env_table.get(k) {
            return Some(v.clone());
        }
        // Fall back to the process env so non-secret values can
        // come from a long-lived shell while secrets live in the
        // overlay file.
        read_env(k)
    };
    MarketRegistry::from_toml_str_with_env(&toml_text, lookup)
        .map_err(|e| format!("markets parse failed: {e}"))
}

/// Wave 20 — read process stdin to a `String`. Helper for the
/// real CLI path; tests use `read_stdin = || Ok("...".into())`.
pub fn read_process_stdin() -> Result<String, String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("stdin read failed: {e}"))?;
    Ok(buf)
}

/// Wave 20 — argument-parsing helper for the prober / scan modes.
/// Pulls `--markets-stdin` and `--env-from-file=PATH` out of the
/// raw arg vector and returns the remaining positional args.
///
/// Returns `(MarketsSource, EnvSource, positional_remaining)`.
pub fn extract_sources(
    args: impl IntoIterator<Item = String>,
) -> (MarketsSource, EnvSource, Vec<String>, Vec<String>) {
    let mut positional: Vec<String> = Vec::new();
    let mut markets: Option<MarketsSource> = None;
    let mut env: Option<EnvSource> = None;
    let mut errors: Vec<String> = Vec::new();
    for a in args {
        if a == "--markets-stdin" {
            if markets.is_some() {
                errors.push("--markets-stdin specified twice".into());
            }
            markets = Some(MarketsSource::Stdin);
            continue;
        }
        if let Some(rest) = a.strip_prefix("--env-from-file=") {
            if rest.is_empty() {
                errors.push("--env-from-file= requires a path".into());
            } else if env.is_some() {
                errors.push("--env-from-file specified twice".into());
            } else {
                env = Some(EnvSource::File(rest.to_string()));
            }
            continue;
        }
        if a == "--env-from-file" {
            errors.push("--env-from-file requires =PATH form".into());
            continue;
        }
        if a.starts_with("--") {
            errors.push(format!("unknown flag `{a}`"));
            continue;
        }
        positional.push(a);
    }
    let markets = markets.unwrap_or_else(|| {
        // Default — first positional becomes the markets path.
        // We pop it so subsequent positional consumers see the
        // wave-19 shape. Caller is responsible for handling the
        // missing-arg case.
        if positional.is_empty() {
            MarketsSource::File(String::new())
        } else {
            let p = positional.remove(0);
            MarketsSource::File(p)
        }
    });
    let env = env.unwrap_or(EnvSource::Process);
    (markets, env, positional, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBKEY_A: &str = "11111111111111111111111111111112";
    const PUBKEY_B: &str = "Sysvar1nstructions1111111111111111111111111";
    const PUBKEY_C: &str = "SysvarC1ock11111111111111111111111111111111";
    const VALID_LEADER: &str = "9rnyx9hcsuVB1bdmgqkLPGqgFmXJ8tJjwoLg6h59qXCV";

    fn fixture_toml() -> String {
        format!(
            "[[markets]]\n\
            symbol = \"SOL-USD\"\n\
            program_id = \"{a}\"\n\
            market_pda = \"{b}\"\n\
            lock_pda   = \"{c}\"\n\
            expected_leader = \"${{EXPECTED_LEADER_SOL_USD}}\"\n",
            a = PUBKEY_A,
            b = PUBKEY_B,
            c = PUBKEY_C,
        )
    }

    #[test]
    fn parse_env_file_handles_basic_lines() {
        let text = "FOO=bar\nBAZ=qux\n";
        let m = parse_env_file(text).unwrap();
        assert_eq!(m.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(m.get("BAZ"), Some(&"qux".to_string()));
    }

    #[test]
    fn parse_env_file_strips_export_prefix() {
        let m = parse_env_file("export FOO=bar\n").unwrap();
        assert_eq!(m.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn parse_env_file_strips_double_quotes() {
        let m = parse_env_file(r#"FOO="hello world""#).unwrap();
        assert_eq!(m.get("FOO"), Some(&"hello world".to_string()));
    }

    #[test]
    fn parse_env_file_skips_comments_and_blanks() {
        let text = "# top comment\n\nFOO=bar\n   # indented\nBAZ=qux\n";
        let m = parse_env_file(text).unwrap();
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn parse_env_file_rejects_missing_equals() {
        let err = parse_env_file("FOO\n").unwrap_err();
        assert!(err.contains("missing `=`"));
    }

    #[test]
    fn parse_env_file_rejects_empty_key() {
        let err = parse_env_file("=value\n").unwrap_err();
        assert!(err.contains("empty key"));
    }

    #[test]
    fn parse_env_file_rejects_invalid_key() {
        let err = parse_env_file("3FOO=value\n").unwrap_err();
        assert!(err.contains("invalid key"));
    }

    #[test]
    fn extract_sources_picks_up_stdin_flag() {
        let args = vec![
            "--markets-stdin".to_string(),
            "/tmp/prom".to_string(),
            "/tmp/json".to_string(),
        ];
        let (m, e, pos, errs) = extract_sources(args);
        assert_eq!(m, MarketsSource::Stdin);
        assert_eq!(e, EnvSource::Process);
        assert_eq!(pos, vec!["/tmp/prom".to_string(), "/tmp/json".to_string()]);
        assert!(errs.is_empty());
    }

    #[test]
    fn extract_sources_picks_up_env_from_file_flag() {
        let args = vec![
            "/etc/markets.toml".to_string(),
            "--env-from-file=/run/secrets/prober.env".to_string(),
            "/tmp/prom".to_string(),
            "/tmp/json".to_string(),
        ];
        let (m, e, pos, errs) = extract_sources(args);
        assert_eq!(m, MarketsSource::File("/etc/markets.toml".into()));
        assert_eq!(e, EnvSource::File("/run/secrets/prober.env".into()));
        assert_eq!(pos.len(), 2);
        assert!(errs.is_empty());
    }

    #[test]
    fn extract_sources_rejects_unknown_flag() {
        let args = vec!["--bogus".to_string()];
        let (_, _, _, errs) = extract_sources(args);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("unknown flag"));
    }

    #[test]
    fn extract_sources_rejects_double_specified_stdin() {
        let args = vec![
            "--markets-stdin".to_string(),
            "--markets-stdin".to_string(),
        ];
        let (_, _, _, errs) = extract_sources(args);
        assert!(errs.iter().any(|e| e.contains("twice")));
    }

    #[test]
    fn extract_sources_rejects_env_from_file_without_path() {
        let args = vec!["--env-from-file".to_string()];
        let (_, _, _, errs) = extract_sources(args);
        assert!(errs.iter().any(|e| e.contains("=PATH")));
    }

    #[test]
    fn load_registry_uses_inline_env_table() {
        let mut env = HashMap::new();
        env.insert(
            "EXPECTED_LEADER_SOL_USD".to_string(),
            VALID_LEADER.to_string(),
        );
        let reg = load_registry(
            &MarketsSource::Stdin,
            &EnvSource::Inline(env),
            |_| panic!("read_path should not be called"),
            || Ok(fixture_toml()),
            |_| panic!("read_env should not be called"),
        )
        .expect("registry");
        assert_eq!(reg.len(), 1);
        assert!(reg.iter().next().unwrap().expected_leader.is_some());
    }

    #[test]
    fn load_registry_falls_back_to_process_env_for_unknown_keys() {
        use std::cell::RefCell;
        let env = HashMap::new(); // empty overlay
        let process_calls: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let reg = load_registry(
            &MarketsSource::Stdin,
            &EnvSource::Inline(env),
            |_| panic!("no file"),
            || Ok(fixture_toml()),
            |k| {
                process_calls.borrow_mut().push(k.to_string());
                Some(VALID_LEADER.to_string())
            },
        )
        .expect("registry");
        assert_eq!(reg.len(), 1);
        assert!(process_calls
            .borrow()
            .contains(&"EXPECTED_LEADER_SOL_USD".to_string()));
    }

    #[test]
    fn load_registry_returns_error_when_env_unset() {
        let env = HashMap::new();
        let err = load_registry(
            &MarketsSource::Stdin,
            &EnvSource::Inline(env),
            |_| panic!("no file"),
            || Ok(fixture_toml()),
            |_| None,
        )
        .expect_err("should error");
        assert!(err.contains("EXPECTED_LEADER_SOL_USD"));
    }

    #[test]
    fn load_registry_uses_env_file_overlay() {
        let env_file_text = format!("EXPECTED_LEADER_SOL_USD={VALID_LEADER}\n");
        let reg = load_registry(
            &MarketsSource::File("/markets.toml".into()),
            &EnvSource::File("/env.file".into()),
            |path| match path {
                "/markets.toml" => Ok(fixture_toml()),
                "/env.file" => Ok(env_file_text.clone()),
                _ => Err(format!("unexpected path {path}")),
            },
            || panic!("stdin should not be read"),
            |_| panic!("process env should not be consulted"),
        )
        .expect("registry");
        assert_eq!(reg.len(), 1);
    }
}
