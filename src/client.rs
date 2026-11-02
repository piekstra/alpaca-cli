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
//!
//! Written once against pk-cli-core's [`Transport`], so the terminal binary
//! (`pk_cli_http::BlockingTransport`) and the Cloudflare Worker module
//! (`pk_cli_mcp::wasm::HostTransport`, the Worker's `fetch`) run this same
//! request building and error mapping. Natively the futures finish on their
//! first poll; the commands drive them with `pk_cli_core::block_on`.

use pk_cli_core::transport::{HttpRequest, HttpResponse, Transport};
use pk_cli_core::CliError;
use serde_json::Value;

use crate::config::BIN;
use crate::credential::ApiKeys;

/// The Alpaca Trading API client over any transport: blocking reqwest
/// natively, the Worker's `fetch` in the module.
pub struct Alpaca<T> {
    transport: T,
    base_url: String,
    keys: ApiKeys,
    verbose: bool,
    rejected_key_hint: Option<&'static str>,
    /// Natively, the time of the last request, for [`MIN_REQUEST_INTERVAL`].
    /// The Worker has no clock to sleep on and makes a few requests a call.
    #[cfg(not(target_arch = "wasm32"))]
    pacer: Option<std::cell::RefCell<Option<std::time::Instant>>>,
}

/// Minimum spacing between the binary's requests. Alpaca's documented budget
/// is 200 requests/minute per key on the standard plan (docs/api.md §4);
/// 250 ms is a polite 4/s burst floor that no CLI invocation here ever
/// sustains long enough to matter.
#[cfg(not(target_arch = "wasm32"))]
pub const MIN_REQUEST_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[cfg(feature = "native")]
impl Alpaca<pk_cli_http::BlockingTransport> {
    /// The binary's client: blocking HTTP, paced, and auth errors that name
    /// the commands that fix them.
    pub fn new(base_url: String, keys: ApiKeys, verbose: bool) -> Result<Self, CliError> {
        let mut client =
            Self::with_transport(&base_url, keys, pk_cli_http::BlockingTransport::new()?);
        client.verbose = verbose;
        client.rejected_key_hint =
            Some("check the environment (`alp config show`) and re-run `alp auth login`");
        client.pacer = Some(std::cell::RefCell::new(None));
        Ok(client)
    }
}

impl<T: Transport> Alpaca<T> {
    /// A client over any transport. Error messages name no `alp` command:
    /// the Worker builds its client this way, where the keys live in the
    /// Worker's secrets, not a keychain.
    pub fn with_transport(base_url: &str, keys: ApiKeys, transport: T) -> Self {
        Self {
            transport,
            base_url: base_url.trim_end_matches('/').to_string(),
            keys,
            verbose: false,
            rejected_key_hint: None,
            #[cfg(not(target_arch = "wasm32"))]
            pacer: None,
        }
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

    /// The GET for a path (leading slash, relative to the API host) with
    /// optional query parameters, without sending it, so a test can see
    /// exactly what goes on the wire.
    pub fn request(&self, path: &str, query: &[(&str, String)]) -> HttpRequest {
        HttpRequest::get(format!("{}{path}", self.base_url))
            .query(query)
            .header("APCA-API-KEY-ID", &self.keys.key_id)
            .header("APCA-API-SECRET-KEY", &self.keys.secret_key)
            .header("Accept", "application/json")
            // On the request, so the binary and the Worker send the same one.
            .user_agent(BIN, env!("CARGO_PKG_VERSION"))
    }

    /// GET a path with optional query parameters (see [`Self::request`]).
    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, CliError> {
        self.pace();
        if self.verbose {
            // The path and parameters, never the keys.
            eprintln!("GET {}{path} {query:?}", self.base_url);
        }
        let resp = self.transport.send(self.request(path, query)).await?;
        response_json(&resp, self.rejected_key_hint)
    }

    /// Natively, sleep just long enough to keep under
    /// [`MIN_REQUEST_INTERVAL`].
    #[cfg(not(target_arch = "wasm32"))]
    fn pace(&self) {
        let Some(pacer) = &self.pacer else { return };
        let mut last = pacer.borrow_mut();
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < MIN_REQUEST_INTERVAL {
                std::thread::sleep(MIN_REQUEST_INTERVAL - elapsed);
            }
        }
        *last = Some(std::time::Instant::now());
    }

    #[cfg(target_arch = "wasm32")]
    fn pace(&self) {}
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
/// found (4), 429 and other non-2xx (a redirect included: none is followed)
/// → upstream (5); then parse JSON.
///
/// Alpaca's error bodies are `{"code": 40110000, "message": "..."}`; the
/// message is surfaced because "HTTP 403" alone does not distinguish revoked
/// keys from an account restriction. `hint` is the binary's remedy for
/// rejected keys; the Worker's comes from the MCP host instead.
pub fn response_json(resp: &HttpResponse, hint: Option<&str>) -> Result<Value, CliError> {
    let status = resp.status;
    if resp.is_success() {
        return serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Upstream(format!("parsing provider JSON: {e}")));
    }
    let detail = body_message(&resp.body);
    match status {
        401 | 403 => Err(CliError::Auth(match hint {
            Some(h) => format!("Alpaca rejected the API keys (HTTP {status}{detail}) — {h}"),
            None => format!("Alpaca rejected the API keys (HTTP {status}{detail})"),
        })),
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
fn body_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
        .map(|m| format!(": {m}"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Unsent;

    impl Transport for Unsent {
        async fn send(&self, _: HttpRequest) -> Result<HttpResponse, CliError> {
            unreachable!("these tests build requests without sending them")
        }
    }

    fn client() -> Alpaca<Unsent> {
        Alpaca::with_transport(
            "https://api.alpaca.markets/",
            ApiKeys {
                key_id: "AKDUMMY000000000001".into(),
                secret_key: "DUMMYSECRET0001".into(),
            },
            Unsent,
        )
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

    #[test]
    fn a_request_carries_the_key_headers_and_the_family_user_agent() {
        let r = client().request("/v2/account/activities", &[("page_size", "100".into())]);
        assert_eq!(r.method, "GET");
        assert_eq!(
            r.url,
            "https://api.alpaca.markets/v2/account/activities?page_size=100"
        );
        assert_eq!(
            r.header_value("APCA-API-KEY-ID"),
            Some("AKDUMMY000000000001")
        );
        assert_eq!(
            r.header_value("APCA-API-SECRET-KEY"),
            Some("DUMMYSECRET0001")
        );
        assert_eq!(r.header_value("Accept"), Some("application/json"));
        assert_eq!(
            r.header_value("User-Agent"),
            Some(concat!("alp/", env!("CARGO_PKG_VERSION")))
        );
        // Debug shows header names only, never the secret.
        assert!(!format!("{r:?}").contains("DUMMYSECRET0001"));
    }

    #[test]
    fn statuses_map_onto_the_exit_code_contract() {
        let body = r#"{"code": 40110000, "message": "request is not authorized"}"#;
        let e = response_json(&HttpResponse::new(401, body), None).unwrap_err();
        assert_eq!(e.exit_code(), 3);
        assert!(e.to_string().contains("request is not authorized"), "{e}");
        assert!(
            !e.to_string().contains("alp "),
            "no command without a hint: {e}"
        );
        let e = response_json(
            &HttpResponse::new(403, "{}"),
            Some("re-run `alp auth login`"),
        )
        .unwrap_err();
        assert!(e.to_string().ends_with("re-run `alp auth login`"), "{e}");
        assert_eq!(
            response_json(&HttpResponse::new(404, ""), None)
                .unwrap_err()
                .exit_code(),
            4
        );
        for status in [429, 500, 302] {
            assert_eq!(
                response_json(&HttpResponse::new(status, "<html>"), None)
                    .unwrap_err()
                    .exit_code(),
                5,
                "{status}"
            );
        }
        let v = response_json(&HttpResponse::new(200, r#"{"cash":"1.00"}"#), None).unwrap();
        assert_eq!(v["cash"], "1.00");
        let e = response_json(&HttpResponse::new(200, "not json"), None).unwrap_err();
        assert_eq!(e.exit_code(), 5);
    }
}
