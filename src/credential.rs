//! The one secret this CLI holds: an Alpaca API key pair (key ID + secret
//! key), stored as a single OS-keychain entry under `piekstra.alp`, keyed by
//! environment (`live` / `paper`) because the two environments issue
//! different, non-interchangeable pairs.
//!
//! [`ApiKeys`] implements [`std::fmt::Debug`] **by hand** so a stray `{:?}`
//! in a future debug session cannot print the secret key to a terminal or a
//! log file.

use pk_cli_core::CliError;
use pk_cli_secrets::{CredentialStore, Secret};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Env;

/// An Alpaca API key pair, as issued on the dashboard's "API Keys" panel.
#[derive(Clone, Serialize, Deserialize)]
pub struct ApiKeys {
    pub key_id: String,
    pub secret_key: String,
}

impl std::fmt::Debug for ApiKeys {
    /// Hand-written: the derived impl would print the secret key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeys")
            .field("key_id", &self.masked_key_id())
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

impl ApiKeys {
    /// Parse the key pair from whatever shape the owner pasted.
    ///
    /// The Alpaca dashboard shows the two values in a web panel rather than
    /// offering a canonical file, so every user arrives with a different
    /// transcription. Three shapes are accepted:
    ///
    /// - JSON: `{"key_id": "...", "secret_key": "..."}`, also accepting the
    ///   env-var spellings `APCA_API_KEY_ID`/`APCA_API_SECRET_KEY` and the
    ///   SDK spellings `api_key`/`api_secret`.
    /// - `KEYID:SECRET` on one line.
    /// - The key ID and the secret key on two lines.
    ///
    /// Being liberal here is not sloppiness: the alternative is a user who
    /// stores a subtly wrong credential and then debugs a bare `40110000
    /// request is not authorized` from Alpaca, which says nothing about which
    /// half was wrong.
    pub fn parse(raw: &str) -> Result<Self, CliError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(CliError::Usage("empty API key credential".into()));
        }

        if raw.starts_with('{') {
            let v: Value = serde_json::from_str(raw)
                .map_err(|e| CliError::Usage(format!("credential is not valid JSON: {e}")))?;
            let key = first_str(
                &v,
                &["key_id", "APCA_API_KEY_ID", "api_key", "apiKey", "key"],
            );
            let secret = first_str(
                &v,
                &[
                    "secret_key",
                    "APCA_API_SECRET_KEY",
                    "api_secret",
                    "apiSecret",
                    "secret",
                ],
            );
            return match (key, secret) {
                (Some(k), Some(s)) => Self::checked(k, s),
                _ => Err(CliError::Usage(
                    "credential JSON needs both a key ID and a secret key \
                     (keys: key_id/secret_key or APCA_API_KEY_ID/APCA_API_SECRET_KEY)"
                        .into(),
                )),
            };
        }

        let lines: Vec<&str> = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        match lines.as_slice() {
            // Two lines: key ID then secret key.
            [k, s] => Self::checked(k.to_string(), s.to_string()),
            // One line: KEYID:SECRET.
            [one] => match one.split_once(':') {
                Some((k, s)) => Self::checked(k.trim().to_string(), s.trim().to_string()),
                None => Err(CliError::Usage(
                    "that looks like a key ID with no secret key — supply \
                     `KEYID:SECRET`, two lines, or the JSON form"
                        .into(),
                )),
            },
            _ => Err(CliError::Usage(
                "could not read a key ID and a secret key from that input".into(),
            )),
        }
    }

    fn checked(key_id: String, secret_key: String) -> Result<Self, CliError> {
        if key_id.is_empty() || secret_key.is_empty() {
            return Err(CliError::Usage(
                "both the key ID and the secret key must be non-empty".into(),
            ));
        }
        Ok(Self { key_id, secret_key })
    }

    /// The key ID with all but its last four characters hidden, for display.
    pub fn masked_key_id(&self) -> String {
        mask(&self.key_id)
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// All but the last four characters replaced, so `auth status` can prove
/// *which* key is stored without printing a credential fragment long enough
/// to matter.
pub fn mask(s: &str) -> String {
    if s.len() <= 4 {
        return "****".into();
    }
    format!("{}{}", "*".repeat(s.len() - 4), &s[s.len() - 4..])
}

fn first_str(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| v.get(k).and_then(Value::as_str))
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Read the stored key pair for an environment. `None` when nothing is stored.
pub fn read(creds: &CredentialStore, env: Env) -> Result<Option<ApiKeys>, CliError> {
    match creds.get(env.as_str())? {
        Some(secret) => {
            let keys: ApiKeys = serde_json::from_str(secret.expose()).map_err(|_| {
                CliError::Keychain(format!(
                    "the stored `{}` credential is not in this tool's format — \
                     re-run `alp auth login --overwrite`",
                    env.as_str()
                ))
            })?;
            Ok(Some(keys))
        }
        None => Ok(None),
    }
}

/// Read the stored key pair, or fail as auth (exit 3) pointing at login.
pub fn require(creds: &CredentialStore, env: Env) -> Result<ApiKeys, CliError> {
    read(creds, env)?.ok_or_else(|| {
        CliError::Auth(format!(
            "no Alpaca API keys stored for the `{}` environment — run `alp auth login`",
            env.as_str()
        ))
    })
}

/// Store the key pair for an environment.
pub fn write(creds: &CredentialStore, env: Env, keys: &ApiKeys) -> Result<(), CliError> {
    creds.set(env.as_str(), &Secret::new(keys.to_json()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_documented_shapes() {
        for raw in [
            "{\"key_id\": \"AKDUMMY000000000001\", \"secret_key\": \"DUMMYSECRET0001\"}",
            "{\"APCA_API_KEY_ID\": \"AKDUMMY000000000001\", \
             \"APCA_API_SECRET_KEY\": \"DUMMYSECRET0001\"}",
            "AKDUMMY000000000001:DUMMYSECRET0001",
            "AKDUMMY000000000001\nDUMMYSECRET0001\n",
        ] {
            let k = ApiKeys::parse(raw).unwrap();
            assert_eq!(k.key_id, "AKDUMMY000000000001");
            assert_eq!(k.secret_key, "DUMMYSECRET0001");
        }
    }

    #[test]
    fn half_a_credential_is_a_usage_error() {
        assert_eq!(ApiKeys::parse("").unwrap_err().exit_code(), 2);
        assert_eq!(
            ApiKeys::parse("AKDUMMY000000000001")
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            ApiKeys::parse("{\"key_id\": \"AKDUMMY000000000001\"}")
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(ApiKeys::parse("a:\n").unwrap_err().exit_code(), 2);
    }

    #[test]
    fn debug_and_mask_never_reveal_the_secret() {
        let k = ApiKeys::parse("AKDUMMY000000000001:DUMMYSECRET0001").unwrap();
        let dbg = format!("{k:?}");
        assert!(!dbg.contains("DUMMYSECRET0001"), "{dbg}");
        assert!(dbg.contains("<redacted>"));
        assert_eq!(mask("AKID1234"), "****1234");
        assert_eq!(mask("abc"), "****");
    }
}
