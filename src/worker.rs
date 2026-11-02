//! The MCP tools as a WebAssembly module for the deploy-your-own Cloudflare
//! Worker (cli-common's `connector/`), released as `alp-mcp-<version>.wasm`
//! and built with `make wasm`.
//!
//! The tools are the ones `alp mcp` serves ([`crate::mcp::AlpacaTools`]),
//! over the same client, reads and mapping; only the transport differs: the
//! Worker's `fetch` ([`pk_cli_mcp::wasm::HostTransport`]). Instructions and
//! auth errors say the keys are in the Worker's secrets, not the keychain.
//!
//! The Worker passes its secrets and vars as each call's config, by name, the
//! same names as the CLI's environment variables:
//!
//! | Key | |
//! |---|---|
//! | `ALP_API_KEY_ID` | the API key ID |
//! | `ALP_API_SECRET_KEY` | its secret key |
//! | `ALP_ENV` | optional: `live` (default) or `paper`; it picks the API host, and must be the environment the key pair was issued for |
//! | `ALP_BASE_URL` | optional: the API host, overriding `ALP_ENV`'s. `https://`, or plain `http://` to a loopback address (a local mock in development) |

use pk_cli_core::dates::{self, Civil};
use pk_cli_core::CliError;
use pk_cli_mcp::wasm::HostTransport;
use serde_json::Value;

use crate::client::Alpaca;
use crate::config::Env;
use crate::credential::{from_vars, ENV_KEY_ID, ENV_SECRET_KEY};
use crate::mcp::{AlpacaTools, ClientSource};

/// The config key for the environment (`live` or `paper`).
pub const ENV: &str = "ALP_ENV";
/// The config key for an API host override.
pub const BASE_URL: &str = "ALP_BASE_URL";

pk_cli_mcp::export_wasm!(tools);

/// The toolset for one call, from the Worker's config. Never fails: without
/// usable settings, each tool call answers an auth error instead, so the
/// tools can be described before any secret is set.
pub fn tools(config: &Value) -> AlpacaTools<ClientSource<HostTransport>> {
    let env = environment(config);
    let client = env.clone().and_then(|env| client(config, env));
    AlpacaTools::with_today(
        ClientSource::new(client),
        env.unwrap_or(Env::Live),
        worker_today,
    )
}

/// A setting's value, trimmed; `None` when absent or blank.
fn text<'a>(config: &'a Value, key: &str) -> Option<&'a str> {
    config
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn environment(config: &Value) -> Result<Env, CliError> {
    match text(config, ENV) {
        None => Ok(Env::Live),
        Some(raw) => Env::parse(raw).map_err(|_| {
            CliError::Auth(format!(
                "the Worker's {ENV} is `{raw}`; set it to `live` or `paper`, the environment \
                 the API keys were issued for"
            ))
        }),
    }
}

fn client(config: &Value, env: Env) -> Result<Alpaca<HostTransport>, CliError> {
    let keys = from_vars(
        text(config, ENV_KEY_ID).map(str::to_string),
        text(config, ENV_SECRET_KEY).map(str::to_string),
    )?
    .ok_or_else(|| {
        CliError::Auth(format!(
            "no Alpaca API keys are configured: the Worker's {ENV_KEY_ID} and {ENV_SECRET_KEY} \
             secrets are not set"
        ))
    })?;
    let base_url = match text(config, BASE_URL) {
        Some(url) => checked_base_url(url)?,
        None => env.default_base().to_string(),
    };
    Ok(Alpaca::with_transport(&base_url, keys, HostTransport))
}

/// An `ALP_BASE_URL`: `https://`, or plain `http://` to a loopback address
/// for a local mock. The keys travel in plain headers, so nothing else is
/// accepted.
fn checked_base_url(url: &str) -> Result<String, CliError> {
    let url = url.trim_end_matches('/');
    if url.starts_with("https://") || loopback_http(url) {
        return Ok(url.to_string());
    }
    Err(CliError::Auth(format!(
        "the Worker's {BASE_URL} must start with https:// (the API keys travel in its request \
         headers); got `{url}`"
    )))
}

/// Whether `url` is plain HTTP to this machine: the host, with any port, is
/// `127.0.0.1`, `localhost` or `[::1]`. Userinfo (`http://127.0.0.1:1@host`)
/// leaves the host somewhere else, so it does not qualify.
fn loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => authority,
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// Today in UTC by the Worker's clock (`Date.now()`).
fn worker_today() -> Civil {
    dates::civil_from_unix((pk_cli_mcp::wasm::now_ms() / 1000.0) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_or_loopback_http_may_carry_the_keys() {
        for ok in [
            "https://api.alpaca.markets",
            "https://paper-api.alpaca.markets/",
            "http://127.0.0.1:8789",
            "http://localhost:8789/",
            "http://[::1]:8789",
            "http://127.0.0.1",
        ] {
            assert!(checked_base_url(ok).is_ok(), "{ok}");
        }
        assert_eq!(
            checked_base_url("http://127.0.0.1:8789/").unwrap(),
            "http://127.0.0.1:8789"
        );
        for bad in [
            "http://api.alpaca.markets",
            "http://localhost.evil.example",
            "http://127.0.0.1:80@evil.example",
            "http://127.0.0.1@evil.example:8789",
            "http://127.0.0.2:8789",
            "ftp://127.0.0.1",
            "api.alpaca.markets",
        ] {
            let e = checked_base_url(bad).unwrap_err();
            assert_eq!(e.exit_code(), 3, "{bad}");
            assert!(e.to_string().contains(BASE_URL), "{e}");
        }
    }

    #[test]
    fn the_environment_defaults_to_live_and_names_itself_when_wrong() {
        assert_eq!(environment(&Value::Null).unwrap(), Env::Live);
        let paper = serde_json::json!({ ENV: " paper " });
        assert_eq!(environment(&paper).unwrap(), Env::Paper);
        let e = environment(&serde_json::json!({ ENV: "prod" })).unwrap_err();
        assert_eq!(e.exit_code(), 3);
        assert!(e.to_string().contains("ALP_ENV is `prod`"), "{e}");
    }

    #[test]
    fn today_is_the_worker_clock_s_utc_date() {
        let (y, m, d) = worker_today();
        assert!(y >= 2026 && (1..=12).contains(&m) && (1..=31).contains(&d));
    }
}
