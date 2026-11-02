//! One module per command group, plus the tiny JSON helpers they share.

pub mod account;
pub mod activities;
pub mod api;
pub mod auth;
pub mod misc;
pub mod positions;

use pk_cli_core::{CliError, CommonArgs};

use crate::client::Alpaca;
use crate::config::Env;

/// Everything a provider-reading command needs, built only after arguments
/// validate and the credential resolves.
pub struct Ctx {
    pub common: CommonArgs,
    pub env: Env,
    pub api: Alpaca,
}

impl Ctx {
    pub fn new(common: CommonArgs, env: Env, api: Alpaca) -> Result<Self, CliError> {
        Ok(Ctx { common, env, api })
    }
}

/// A string field, `None` when absent or not a string.
pub fn str_at(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Inclusive ISO-date range check. ISO `YYYY-MM-DD` strings order
/// lexicographically, so plain string comparison is exact.
pub fn date_within(date: &str, since: Option<&str>, until: Option<&str>) -> bool {
    if let Some(s) = since {
        if date < s {
            return false;
        }
    }
    if let Some(u) = until {
        if date > u {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_check_is_inclusive_on_both_ends() {
        assert!(date_within(
            "2026-03-14",
            Some("2026-03-14"),
            Some("2026-03-14")
        ));
        assert!(date_within("2026-03-14", None, None));
        assert!(!date_within("2026-03-13", Some("2026-03-14"), None));
        assert!(!date_within("2026-03-15", None, Some("2026-03-14")));
    }
}
