//! Non-secret settings: which Alpaca environment (`live` or `paper`) to talk
//! to, and an explicit base-URL override for the day Alpaca moves a host.
//!
//! Everything that authenticates — the API key ID and secret key — lives in
//! the OS keychain under `piekstra.alp`, keyed by environment (see
//! [`crate::credential`]). The config file holds preferences only, and a test
//! enforces that it can never accept a secret-shaped key.

use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};

/// Binary name: keychain service (`piekstra.alp`), config dir, env prefix.
pub const BIN: &str = "alp";
/// Repo, for `info` and `self-update`.
pub const REPO: &str = "piekstra/alpaca-cli";

/// The official Trading API hosts (docs.alpaca.markets, verified 2026-08-27 —
/// see docs/api.md).
pub const LIVE_BASE: &str = "https://api.alpaca.markets";
pub const PAPER_BASE: &str = "https://paper-api.alpaca.markets";

/// The two Alpaca environments. Live and paper accounts have **different**
/// API key pairs, so the environment names both the base URL and the keychain
/// slot the credential lives in — switching env can never silently send one
/// environment's keys at the other's host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    Live,
    Paper,
}

impl Env {
    pub fn parse(raw: &str) -> Result<Self, CliError> {
        match raw {
            "live" => Ok(Env::Live),
            "paper" => Ok(Env::Paper),
            other => Err(CliError::Usage(format!(
                "unknown environment `{other}` (known: live, paper)"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Env::Live => "live",
            Env::Paper => "paper",
        }
    }

    pub fn default_base(&self) -> &'static str {
        match self {
            Env::Live => LIVE_BASE,
            Env::Paper => PAPER_BASE,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// `live` (default) or `paper`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Explicit base-URL override; wins over `env` when set. HTTPS only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Config {
    /// The environment in effect: the `--env` flag (or `ALP_ENV`) beats the
    /// config file beats the default, per SPEC v1 §1.1 precedence.
    pub fn env(&self, flag: Option<&str>) -> Result<Env, CliError> {
        match flag.or(self.env.as_deref()) {
            Some(raw) => Env::parse(raw),
            None => Ok(Env::Live),
        }
    }

    /// The base URL in effect: explicit `base_url` override, else the
    /// environment's official host.
    pub fn base_url(&self, flag_env: Option<&str>) -> Result<String, CliError> {
        if let Some(url) = &self.base_url {
            return Ok(url.trim_end_matches('/').to_string());
        }
        Ok(self.env(flag_env)?.default_base().to_string())
    }
}

/// Validate a base-URL override. HTTPS only: this URL receives the account's
/// API keys in headers, and the config file is the easiest place for a
/// downgrade to hide.
pub fn validate_base_url(url: &str) -> Result<(), CliError> {
    if url.starts_with("https://") {
        Ok(())
    } else {
        Err(CliError::Usage(
            "base_url must start with https:// — the API keys travel in headers".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_environment_is_live() {
        let cfg = Config::default();
        assert_eq!(cfg.env(None).unwrap(), Env::Live);
        assert_eq!(cfg.base_url(None).unwrap(), LIVE_BASE);
    }

    #[test]
    fn the_env_flag_beats_the_config_file() {
        let cfg = Config {
            env: Some("live".into()),
            base_url: None,
        };
        assert_eq!(cfg.env(Some("paper")).unwrap(), Env::Paper);
        assert_eq!(cfg.base_url(Some("paper")).unwrap(), PAPER_BASE);
    }

    #[test]
    fn a_base_url_override_wins_and_is_normalized() {
        let cfg = Config {
            env: Some("paper".into()),
            base_url: Some("https://example.test/".into()),
        };
        assert_eq!(cfg.base_url(None).unwrap(), "https://example.test");
    }

    #[test]
    fn unknown_environments_are_usage_errors() {
        assert_eq!(Env::parse("prod").unwrap_err().exit_code(), 2);
        let cfg = Config {
            env: Some("prod".into()),
            base_url: None,
        };
        assert!(cfg.env(None).is_err());
    }

    #[test]
    fn plain_http_base_urls_are_refused() {
        assert!(validate_base_url("https://api.alpaca.markets").is_ok());
        assert_eq!(
            validate_base_url("http://api.alpaca.markets")
                .unwrap_err()
                .exit_code(),
            2
        );
    }
}
