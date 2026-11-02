//! The Alpaca HTTP client: attach the key headers, send a GET, map the answer
//! onto the family's exit-code contract.
//!
//! Deliberately GET-only. There is no `post`/`put`/`delete` on this type, so
//! "this CLI is read-only" is a property of the code rather than a promise in
//! a README. That matters more here than for most providers: the same base
//! URL and the same key pair that read a position will also **place a live
//! order** (`POST /v2/orders`) or liquidate one. The only thing standing
//! between this binary and someone's money is the absence of a method on this
//! struct, so adding a write means adding one here — a visible change, in the
//! same commit as the docs.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use pk_cli_core::CliError;
use serde_json::Value;

use crate::credential::ApiKeys;

/// Minimum spacing between requests. Alpaca's documented budget is 200
/// requests/minute per key on the standard plan (docs/api.md §4); 250 ms is
/// a polite 4/s burst floor that no CLI invocation here ever sustains long
/// enough to matter.
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(250);

pub struct Alpaca {
    http: reqwest::blocking::Client,
    base_url: String,
    keys: ApiKeys,
    last_request: RefCell<Option<Instant>>,
    verbose: bool,
}

impl Alpaca {
    pub fn new(base_url: String, keys: ApiKeys, verbose: bool) -> Result<Self, CliError> {
        Ok(Self {
            http: pk_cli_http::client(crate::config::BIN, env!("CARGO_PKG_VERSION"))?,
            base_url,
            keys,
            last_request: RefCell::new(None),
            verbose,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Whether a caller-supplied absolute URL stays on the configured API
    /// origin. Anything dereferenced by this client carries the account's key
    /// headers, so an off-origin URL would hand the credential to a stranger.
    pub fn same_origin(&self, url: &str) -> bool {
        same_origin(&self.base_url, url)
    }

    /// GET a path (leading slash, relative to the API host) with optional
    /// query parameters.
    pub fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, CliError> {
        self.pace();
        if self.verbose {
            // The path and parameters, never the keys.
            eprintln!("GET {}{path} {query:?}", self.base_url);
        }
        let resp = self
            .http
            .get(format!("{}{path}", self.base_url))
            .query(query)
            .header("APCA-API-KEY-ID", &self.keys.key_id)
            .header("APCA-API-SECRET-KEY", &self.keys.secret_key)
            .header("Accept", "application/json")
            .send()?;
        response_json(resp)
    }

    /// Sleep just long enough to keep under [`MIN_REQUEST_INTERVAL`].
    fn pace(&self) {
        let mut last = self.last_request.borrow_mut();
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < MIN_REQUEST_INTERVAL {
                std::thread::sleep(MIN_REQUEST_INTERVAL - elapsed);
            }
        }
        *last = Some(Instant::now());
    }
}

/// Whether `url` stays on `base`'s origin: exactly the base, or the base
/// followed by a path or query — never a longer hostname that merely starts
/// with the same characters.
pub fn same_origin(base: &str, url: &str) -> bool {
    match url.strip_prefix(base) {
        Some(rest) => rest.is_empty() || rest.starts_with('/') || rest.starts_with('?'),
        None => false,
    }
}

/// Map a response onto the exit-code contract: 401/403 → auth (3), 404 → not
/// found (4), 429 and other non-2xx → upstream (5); then parse JSON.
///
/// Alpaca's error bodies are `{"code": 40110000, "message": "..."}`; the
/// message is surfaced because "HTTP 403" alone does not distinguish revoked
/// keys from an account restriction.
fn response_json(resp: reqwest::blocking::Response) -> Result<Value, CliError> {
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return resp
            .json::<Value>()
            .map_err(|e| CliError::Upstream(format!("parsing provider JSON: {e}")));
    }
    let detail = body_message(resp);
    match status {
        401 | 403 => Err(CliError::Auth(format!(
            "Alpaca rejected the API keys (HTTP {status}{detail}) — check the environment \
             (`alp config show`) and re-run `alp auth login`"
        ))),
        404 => Err(CliError::NotFound(format!("HTTP 404 from Alpaca{detail}"))),
        429 => Err(CliError::Upstream(format!(
            "rate limited by Alpaca (HTTP 429{detail}) — wait a minute and retry"
        ))),
        _ => Err(CliError::Upstream(format!(
            "Alpaca returned HTTP {status}{detail}"
        ))),
    }
}

/// `: message` from an Alpaca error body, or empty. Never the whole body:
/// an unexpected payload could be arbitrarily large or carry echoes of the
/// request.
fn body_message(resp: reqwest::blocking::Response) -> String {
    resp.json::<Value>()
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
        .map(|m| format!(": {m}"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Alpaca {
        Alpaca::new(
            "https://api.alpaca.markets".into(),
            ApiKeys {
                key_id: "AKDUMMY000000000001".into(),
                secret_key: "DUMMYSECRET0001".into(),
            },
            false,
        )
        .unwrap()
    }

    /// The origin check guards the `api` passthrough: keys must never be sent
    /// to a host that merely *starts with* the API origin's string.
    #[test]
    fn same_origin_rejects_lookalike_hosts() {
        let c = client();
        assert!(c.same_origin("https://api.alpaca.markets"));
        assert!(c.same_origin("https://api.alpaca.markets/v2/account"));
        assert!(c.same_origin("https://api.alpaca.markets?x=1"));
        assert!(!c.same_origin("https://api.alpaca.markets.evil.example/v2/account"));
        assert!(!c.same_origin("https://data.alpaca.markets/v2/stocks"));
        assert!(!c.same_origin("http://api.alpaca.markets/v2/account"));
    }
}
