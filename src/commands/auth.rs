//! `alp auth` — get the API key pair into (and out of) the OS keychain.
//!
//! Alpaca's credential is a long-lived key pair, not a session: there is no
//! token to refresh and nothing to expire, so `login` = validate + store, and
//! `logout` without `--forget` has nothing to clear (it says so instead of
//! pretending). Keys enter via prompt, `--stdin`, or `--from-env` — never
//! argv — and argument problems fail before the keychain is ever opened
//! (SPEC v1 §1.2).

use pk_cli_auth::{AuthMethod, AuthStatus, LoginArgs, SetCredentialArgs};
use pk_cli_core::{block_on, CliError, CommonArgs};
use pk_cli_secrets::{CredentialStore, Secret};

use crate::client::Alpaca;
use crate::config::Env;
use crate::credential::{self, ApiKeys};
use crate::reads::{self, str_at};

pub fn login(
    common: &CommonArgs,
    env: Env,
    base_url: &str,
    creds: &CredentialStore,
    args: &LoginArgs,
) -> Result<(), CliError> {
    // Argument-shape failures first — nothing below runs for bad flags.
    let explicit = args.source.stdin || args.source.from_env.is_some();
    if args.source.stdin && args.source.from_env.is_some() {
        return Err(CliError::Usage(
            "pass exactly one of --stdin or --from-env".into(),
        ));
    }
    if !explicit && args.non_interactive {
        return Err(CliError::Usage(
            "--non-interactive needs the credential from --stdin or --from-env <VAR>".into(),
        ));
    }

    // Acquire and parse the key pair — still no keychain, no network.
    let keys = acquire(args, explicit)?;

    if credential::read(creds, env)?.is_some() && !args.overwrite {
        return Err(CliError::Usage(format!(
            "API keys are already stored for the `{}` environment — pass --overwrite to \
             replace them",
            env.as_str()
        )));
    }

    // Prove the pair works against the environment it will be stored under,
    // so a paper key can't be silently filed as a live credential.
    if !args.no_verify {
        let api = Alpaca::new(base_url.to_string(), keys.clone(), common.verbose)?;
        let account = block_on(reads::account(&api))?;
        if !common.quiet {
            eprintln!(
                "verified against {} — account {} ({})",
                base_url,
                str_at(&account, "account_number").unwrap_or_else(|| "?".into()),
                str_at(&account, "status").unwrap_or_else(|| "?".into()),
            );
        }
    }

    credential::write(creds, env, &keys)?;
    eprintln!(
        "API keys stored in the OS keychain (service piekstra.alp, entry `{}`)",
        env.as_str()
    );
    Ok(())
}

/// Read the pair from the chosen source. Interactively that is two no-echo
/// prompts; from `--stdin`/`--from-env` (or a pipe) it is any of the shapes
/// [`ApiKeys::parse`] accepts.
fn acquire(args: &LoginArgs, explicit: bool) -> Result<ApiKeys, CliError> {
    use std::io::IsTerminal;
    if explicit {
        let secret = args.source.read(None)?;
        return ApiKeys::parse(secret.expose());
    }
    if std::io::stdin().is_terminal() {
        let id = Secret::prompt("API key ID")?;
        let sk = Secret::prompt("API secret key")?;
        return ApiKeys::parse(&format!("{}\n{}", id.expose(), sk.expose()));
    }
    // Piped input with no flag: same as --stdin.
    let secret = pk_cli_secrets::read_stdin()?;
    ApiKeys::parse(secret.expose())
}

#[derive(clap::Args, Debug)]
pub struct StatusArgs {
    /// Spend one request confirming the stored keys still work.
    #[arg(long)]
    pub verify: bool,
}

/// `auth status` (auth-status/v1). Works logged out; without `--verify` it
/// touches only the keychain.
pub fn status(
    common: &CommonArgs,
    env: Env,
    base_url: &str,
    creds: &CredentialStore,
    verify: bool,
) -> Result<(), CliError> {
    // The environment wins over the keychain for every request (DESIGN.md
    // §1.7), so status reports the pair requests will actually send, and
    // fails as they do (exit 3) on half a pair. With a pair in the
    // environment the keychain is not opened, so `credential_in_keychain` is
    // left out rather than guessed.
    let (stored, in_keychain) = match credential::from_env()? {
        Some(keys) => {
            if !common.quiet {
                eprintln!(
                    "using the key pair from {} and {}; requests do not read the keychain",
                    credential::ENV_KEY_ID,
                    credential::ENV_SECRET_KEY
                );
            }
            (Some(keys), None)
        }
        None => {
            let stored = credential::read(creds, env)?;
            let in_keychain = stored.is_some();
            (stored, Some(in_keychain))
        }
    };
    // The credential is an API key pair; `password` is the nearest variant the
    // family's AuthMethod enum offers (like schw's OAuth). `info.auth.method`
    // is the free string and says `api-key`.
    let mut st = AuthStatus::new(true, stored.is_some(), AuthMethod::Password);
    st.username = stored.as_ref().map(ApiKeys::masked_key_id);
    st.credential_in_keychain = in_keychain;

    if verify {
        match stored {
            Some(keys) => {
                let api = Alpaca::new(base_url.to_string(), keys, common.verbose)?;
                match block_on(reads::account(&api)) {
                    Ok(account) => {
                        st.session_valid = Some(true);
                        st.account = str_at(&account, "account_number");
                    }
                    // Rejected keys are a *status*, not a failure of the
                    // status command.
                    Err(CliError::Auth(_)) => {
                        st.session_valid = Some(false);
                        st.authenticated = false;
                    }
                    Err(other) => return Err(other),
                }
            }
            None => st.session_valid = Some(false),
        }
    }

    st.emit(common.json);
    Ok(())
}

/// `auth logout [--forget]`. There is no session to clear; `--forget` removes
/// the stored key pair for the selected environment.
pub fn logout(env: Env, creds: &CredentialStore, forget: bool) -> Result<(), CliError> {
    if forget {
        if creds.delete(env.as_str())? {
            eprintln!("removed the stored `{}` API keys", env.as_str());
        } else {
            eprintln!("no `{}` API keys were stored", env.as_str());
        }
        return Ok(());
    }
    eprintln!(
        "alp keeps no session state — the `{}` API keys remain stored; \
         pass --forget to remove them",
        env.as_str()
    );
    Ok(())
}

/// `auth set-credential` — raw keychain write for rotation/headless setup.
/// Requires an explicit source; never prompts.
pub fn set_credential(
    env: Env,
    creds: &CredentialStore,
    args: &SetCredentialArgs,
) -> Result<(), CliError> {
    // Errors as usage before any keychain read when no source is given.
    let secret = args.source.read(None)?;
    let keys = ApiKeys::parse(secret.expose())?;
    if credential::read(creds, env)?.is_some() && !args.overwrite {
        return Err(CliError::Usage(format!(
            "API keys are already stored for the `{}` environment — pass --overwrite to \
             replace them",
            env.as_str()
        )));
    }
    credential::write(creds, env, &keys)?;
    eprintln!("credential stored (entry `{}`)", env.as_str());
    Ok(())
}
