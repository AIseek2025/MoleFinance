//! Wave 12 — minimal HTTP exposition server for /metrics + /healthz.
//!
//! ## Why hand-rolled
//!
//! `axum` + `hyper` would each pull in tokio, futures, and a few
//! hundred KB of compiled code that we don't otherwise need (the
//! keeper bot's tick loop is intentionally synchronous). This module
//! is two routes, one thread, no dependencies — small enough that
//! the entire request handler fits on one screen.
//!
//! ## Protocol scope
//!
//! Implements just enough HTTP/1.1 to satisfy
//! [Prometheus's scrape contract](https://prometheus.io/docs/instrumenting/exposition_formats/):
//!
//! - `GET /metrics`  → 200, `Content-Type: text/plain; version=0.0.4`
//! - `GET /healthz`  → 200 if booted, 503 otherwise
//! - anything else   → 404
//!
//! No `Connection: keep-alive`, no chunked encoding, no compression.
//! Prometheus scrapers handle this fine; if you want a richer
//! contract (k8s liveness probes, JSON status endpoint), wave 13 can
//! add it.
//!
//! ## Threading model
//!
//! `spawn_metrics_server` returns a `JoinHandle<()>`. The thread
//! polls `shutdown` between connections and exits cleanly. We use
//! `set_nonblocking(true)` + a 200 ms read-timeout so the polling
//! loop is responsive without spinning the CPU.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::metrics::KeeperMetrics;

/// Wave 21 — provider closure for `/metrics-multi`. Returns the
/// per-market JSON snapshot body. Production deployments wire
/// this to `Arc::new(move || registry.lock().render_per_market_json())`
/// (or equivalent under whatever lock primitive owns the
/// multi-market `MarketRegistry`).
pub type MultiMarketJsonProvider = Arc<dyn Fn() -> String + Send + Sync>;

/// Maximum bytes we will read from a single inbound request.
/// Prometheus scrape requests are tiny (~80 bytes); we cap at 4 KB to
/// reject probe attempts that try to exhaust memory.
const MAX_REQUEST_BYTES: usize = 4096;

/// Maximum time we will spend reading one request before giving up
/// and closing the socket. Prometheus scrapes usually complete in
/// < 1 ms; 1s is a generous timeout that still surfaces hangs.
const READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Spawn the metrics server on its own thread. Returns the listening
/// address (potentially with a port resolved from `:0`) and the
/// thread join handle.
pub fn spawn_metrics_server(
    addr: SocketAddr,
    metrics: Arc<KeeperMetrics>,
    shutdown: Arc<AtomicBool>,
) -> std::io::Result<(SocketAddr, JoinHandle<()>)> {
    spawn_metrics_server_with_multi(addr, metrics, None, shutdown)
}

/// Wave 21 — like [`spawn_metrics_server`] but also wires the
/// optional `/metrics-multi` route. Pass `multi = None` for the
/// wave-12 single-market shape (404 on `/metrics-multi`); pass
/// `Some(provider)` to expose per-market JSON.
pub fn spawn_metrics_server_with_multi(
    addr: SocketAddr,
    metrics: Arc<KeeperMetrics>,
    multi: Option<MultiMarketJsonProvider>,
    shutdown: Arc<AtomicBool>,
) -> std::io::Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr)?;
    let actual_addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let handle = std::thread::Builder::new()
        .name("keeper-bot-metrics".to_string())
        .spawn(move || {
            run_server(listener, metrics, multi, shutdown);
        })?;
    Ok((actual_addr, handle))
}

fn run_server(
    listener: TcpListener,
    metrics: Arc<KeeperMetrics>,
    multi: Option<MultiMarketJsonProvider>,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        match listener.accept() {
            Ok((stream, _peer)) => {
                let m = Arc::clone(&metrics);
                let mm = multi.clone();
                if let Err(e) = handle_connection(stream, &m, mm.as_ref()) {
                    tracing::debug!(error = ?e, "metrics request errored");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                tracing::warn!(error = ?e, "metrics listener accept failed");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    metrics: &KeeperMetrics,
    multi: Option<&MultiMarketJsonProvider>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    stream.set_nonblocking(false)?;

    let request_line = read_request_line(&mut stream)?;
    let response = render_response_with_multi(&request_line, metrics, multi);
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Ok(())
}

/// Read up to `MAX_REQUEST_BYTES` until we find `\r\n\r\n` (end of
/// headers) or the limit is reached. We only need the request line —
/// the path determines routing. Headers are discarded.
fn read_request_line(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::with_capacity(256);
    let mut tmp = [0u8; 256];
    loop {
        if buf.len() >= MAX_REQUEST_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "request too large",
            ));
        }
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        // For minimal HTTP, also accept just `\n\n` (some probe
        // tools).
        if buf.windows(2).any(|w| w == b"\n\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let first_line = text
        .lines()
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty request"))?;
    Ok(first_line.to_string())
}

/// Build the HTTP/1.1 response based on the request line.
///
/// Pure function for testability — feed any request line, get the
/// response string. No I/O.
pub fn render_response(request_line: &str, metrics: &KeeperMetrics) -> String {
    render_response_with_multi(request_line, metrics, None)
}

/// Wave 21 — render variant that also handles `/metrics-multi`
/// when a JSON provider is supplied.
pub fn render_response_with_multi(
    request_line: &str,
    metrics: &KeeperMetrics,
    multi: Option<&MultiMarketJsonProvider>,
) -> String {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path_with_query = parts.next().unwrap_or("");
    let path = path_with_query.split('?').next().unwrap_or("");

    if method != "GET" {
        return canned_response(
            "HTTP/1.1 405 Method Not Allowed\r\n",
            "text/plain; charset=utf-8",
            "method not allowed\n",
        );
    }

    match path {
        "/metrics" => {
            let body = metrics.render_prometheus();
            canned_response(
                "HTTP/1.1 200 OK\r\n",
                "text/plain; version=0.0.4; charset=utf-8",
                &body,
            )
        }
        "/metrics-multi" => match multi {
            Some(provider) => {
                let body = provider();
                canned_response(
                    "HTTP/1.1 200 OK\r\n",
                    "application/json; charset=utf-8",
                    &body,
                )
            }
            None => canned_response(
                "HTTP/1.1 404 Not Found\r\n",
                "text/plain; charset=utf-8",
                "/metrics-multi not configured (single-market deployment)\n",
            ),
        },
        "/healthz" => {
            let booted = metrics
                .up_since_unix_secs
                .load(std::sync::atomic::Ordering::Relaxed)
                != 0;
            if booted {
                canned_response(
                    "HTTP/1.1 200 OK\r\n",
                    "application/json",
                    "{\"status\":\"ok\"}\n",
                )
            } else {
                canned_response(
                    "HTTP/1.1 503 Service Unavailable\r\n",
                    "application/json",
                    "{\"status\":\"booting\"}\n",
                )
            }
        }
        _ => canned_response(
            "HTTP/1.1 404 Not Found\r\n",
            "text/plain; charset=utf-8",
            "not found\n",
        ),
    }
}

fn canned_response(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "{status}Content-Type: {content_type}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{KeeperMetrics, LeaderStatus};

    #[test]
    fn metrics_endpoint_returns_200_and_prometheus_body() {
        let m = KeeperMetrics::new();
        m.observe_boot(1_700_000_000);
        m.set_leader_status(LeaderStatus::Leader);
        let resp = render_response("GET /metrics HTTP/1.1", &m);
        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(resp.contains("Content-Type: text/plain; version=0.0.4"));
        assert!(resp.contains("\nkeeper_leader_status 1\n"));
        assert!(resp.contains("\nkeeper_up_since_unix_seconds 1700000000\n"));
    }

    #[test]
    fn healthz_returns_503_before_boot() {
        let m = KeeperMetrics::new();
        let resp = render_response("GET /healthz HTTP/1.1", &m);
        assert!(resp.starts_with("HTTP/1.1 503"));
        assert!(resp.contains("booting"));
    }

    #[test]
    fn healthz_returns_200_after_boot() {
        let m = KeeperMetrics::new();
        m.observe_boot(1);
        let resp = render_response("GET /healthz HTTP/1.1", &m);
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("\"ok\""));
    }

    #[test]
    fn unknown_path_returns_404() {
        let m = KeeperMetrics::new();
        let resp = render_response("GET /api/v1/whatever HTTP/1.1", &m);
        assert!(resp.starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn non_get_returns_405() {
        let m = KeeperMetrics::new();
        let resp = render_response("POST /metrics HTTP/1.1", &m);
        assert!(resp.starts_with("HTTP/1.1 405"));
    }

    #[test]
    fn query_string_is_stripped() {
        let m = KeeperMetrics::new();
        m.observe_boot(1);
        let resp = render_response("GET /metrics?foo=bar HTTP/1.1", &m);
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
    }

    #[test]
    fn metrics_multi_returns_404_when_provider_absent() {
        let m = KeeperMetrics::new();
        m.observe_boot(1);
        let resp = render_response_with_multi("GET /metrics-multi HTTP/1.1", &m, None);
        assert!(resp.starts_with("HTTP/1.1 404"));
        assert!(resp.contains("not configured"));
    }

    #[test]
    fn metrics_multi_returns_200_json_when_provider_present() {
        let m = KeeperMetrics::new();
        m.observe_boot(1_700_000_000);
        let provider: MultiMarketJsonProvider =
            Arc::new(|| "[{\"market\":\"SOL-USD\",\"metrics\":{}}]".to_string());
        let resp = render_response_with_multi(
            "GET /metrics-multi HTTP/1.1",
            &m,
            Some(&provider),
        );
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("Content-Type: application/json"));
        assert!(resp.contains("\"market\":\"SOL-USD\""));
    }

    #[test]
    fn keeper_metrics_render_json_snapshot_emits_camelcase_keys() {
        let m = KeeperMetrics::new();
        m.observe_boot(1_700_000_001);
        m.set_leader_status(LeaderStatus::Leader);
        m.set_wallet_balance_lamports(123_456_789);
        m.set_vol_samples(40);
        // Wave-12 design: `last_applied_vol_milli` defaults to 0
        // (not the warming sentinel `-1`). Push it to the
        // "warming up" state explicitly so the JSON null-branch
        // is what we exercise.
        m.last_applied_vol_milli
            .store(crate::metrics::APPLIED_VOL_NOT_WARM, std::sync::atomic::Ordering::Relaxed);
        let json = m.render_json_snapshot();
        for key in [
            "ticksTotal",
            "actionsSubmittedTotal",
            "actionsFailedTotal",
            "actionsSkippedTotal",
            "initHintsRecordedTotal",
            "snapshotErrorsTotal",
            "lastTickDurationMs",
            "volSamples",
            "lastInitHints",
            "lastActionsPlanned",
            "upSinceUnixSecs",
            "walletBalanceLamports",
            "appliedVolMilli",
            "leaderStatus",
        ] {
            assert!(json.contains(&format!("\"{key}\":")), "missing {key} in {json}");
        }
        assert!(json.contains("\"leaderStatus\":\"leader\""));
        assert!(json.contains("\"upSinceUnixSecs\":1700000001"));
        assert!(json.contains("\"walletBalanceLamports\":123456789"));
        assert!(json.contains("\"volSamples\":40"));
        assert!(json.contains("\"appliedVolMilli\":null"));
        assert!(json.starts_with('{') && json.ends_with('}'));
    }

    #[test]
    fn keeper_metrics_render_json_snapshot_serialises_warmed_vol() {
        let m = KeeperMetrics::new();
        m.last_applied_vol_milli
            .store(857, std::sync::atomic::Ordering::Relaxed); // σ̂ = 0.857
        let json = m.render_json_snapshot();
        assert!(json.contains("\"appliedVolMilli\":857"));
    }

    #[test]
    fn keeper_metrics_render_json_snapshot_default_vol_is_zero_not_null() {
        // The wave-12 bot default starts last_applied_vol_milli
        // at 0 (atomic default), only flipping to -1 once the
        // first tick observes a `None` from the estimator. The
        // JSON path mirrors that: 0 → numeric `0`, only `-1` →
        // `null`. Pin so we don't accidentally re-sentinel `0`.
        let m = KeeperMetrics::new();
        let json = m.render_json_snapshot();
        assert!(json.contains("\"appliedVolMilli\":0"));
    }

    #[test]
    fn malformed_request_returns_404() {
        let m = KeeperMetrics::new();
        // No method, just a path.
        let resp = render_response("/metrics", &m);
        // method "" is not "GET" → 405.
        assert!(resp.starts_with("HTTP/1.1 405"));
    }

    /// End-to-end: bind on `127.0.0.1:0`, hit it from the test
    /// thread, assert the body. This is the integration test that
    /// would catch e.g. a `set_nonblocking` regression.
    #[test]
    fn end_to_end_metrics_scrape() {
        let metrics = Arc::new(KeeperMetrics::new());
        metrics.observe_boot(1_700_000_001);
        let shutdown = Arc::new(AtomicBool::new(false));
        let (addr, handle) = spawn_metrics_server(
            "127.0.0.1:0".parse().unwrap(),
            Arc::clone(&metrics),
            Arc::clone(&shutdown),
        )
        .expect("spawn");

        // Give the listener a moment to start polling.
        std::thread::sleep(Duration::from_millis(50));

        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        assert!(response.contains("HTTP/1.1 200 OK"));
        assert!(response.contains("keeper_ticks_total"));
        assert!(response.contains("\nkeeper_up_since_unix_seconds 1700000001\n"));

        shutdown.store(true, Ordering::Relaxed);
        handle.join().expect("server thread panicked");
    }

    #[test]
    fn end_to_end_healthz_before_boot_returns_503() {
        let metrics = Arc::new(KeeperMetrics::new());
        let shutdown = Arc::new(AtomicBool::new(false));
        let (addr, handle) = spawn_metrics_server(
            "127.0.0.1:0".parse().unwrap(),
            Arc::clone(&metrics),
            Arc::clone(&shutdown),
        )
        .expect("spawn");

        std::thread::sleep(Duration::from_millis(50));

        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        assert!(response.contains("HTTP/1.1 503"));

        shutdown.store(true, Ordering::Relaxed);
        handle.join().expect("server thread panicked");
    }
}
