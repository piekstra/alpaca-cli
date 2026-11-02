//! `info` and `config` — the parts of the standard surface that need neither
//! the provider nor a credential.

use pk_cli_config::ConfigStore;
use pk_cli_core::info::{AuthInfo, CliInfo};
use pk_cli_core::{output, CliError, CommonArgs};

use crate::config::{validate_base_url, Config, Env, BIN, REPO};

/// `alp info` — machine discovery (`cli-info/v1`). Fully offline.
pub fn info(json_mode: bool) -> Result<(), CliError> {
    let info = CliInfo::new(
        BIN,
        env!("CARGO_PKG_VERSION"),
        &format!("https://github.com/{REPO}"),
        AuthInfo {
            required: true,
            method: "api-key".into(),
            login_hint: Some(format!("{BIN} auth login")),
        },
        &["account", "positions", "transactions", "dividends", "api"],
    );
    let v = serde_json::to_value(&info).unwrap_or_default();
    if json_mode {
        output::json(&v);
    } else {
        output::render(&v);
    }
    Ok(())
}

/// The `config` subcommand, expressed without clap so this module does not
/// depend on the command tree.
pub enum ConfigOp<'a> {
    Path,
    Show,
    Set(&'a str, &'a str),
    Unset(&'a str),
}

pub fn config(
    common: &CommonArgs,
    store: &ConfigStore,
    mut cfg: Config,
    op: ConfigOp<'_>,
) -> Result<(), CliError> {
    match op {
        ConfigOp::Path => {
            println!("{}", store.path()?.display());
            Ok(())
        }
        ConfigOp::Show => {
            let v = serde_json::to_value(&cfg).unwrap_or_default();
            if common.json {
                output::json(&v);
            } else {
                output::render(&v);
            }
            Ok(())
        }
        ConfigOp::Set(key, value) => {
            match key {
                "env" => {
                    Env::parse(value)?;
                    cfg.env = Some(value.to_string());
                }
                "base_url" => {
                    validate_base_url(value)?;
                    cfg.base_url = Some(value.trim_end_matches('/').to_string());
                }
                other => return Err(unknown_key(other)),
            }
            store.save(&cfg)
        }
        ConfigOp::Unset(key) => {
            match key {
                "env" => cfg.env = None,
                "base_url" => cfg.base_url = None,
                other => return Err(unknown_key(other)),
            }
            store.save(&cfg)
        }
    }
}

/// The config file holds preferences only — a secret-shaped key must be
/// refused with the message saying where secrets actually go.
fn unknown_key(key: &str) -> CliError {
    CliError::Usage(format!(
        "unknown config key `{key}` (known: env, base_url; API keys go in the OS \
         keychain via `alp auth login`)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_declares_the_spec_and_the_api_key_method() {
        let info = CliInfo::new(
            BIN,
            "0.1.0",
            "https://github.com/piekstra/alpaca-cli",
            AuthInfo {
                required: true,
                method: "api-key".into(),
                login_hint: None,
            },
            &["account"],
        );
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["schema"], "cli-info/v1");
        assert_eq!(v["spec"], "piekstra-cli/1");
        assert_eq!(v["name"], "alp");
        assert_eq!(v["auth"]["method"], "api-key");
    }
}
