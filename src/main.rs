//! `alp` — read-only CLI for Alpaca brokerage accounts, over the official
//! Trading API (docs.alpaca.markets).
//!
//! Conforms to `piekstra-cli/1` (see `cli-common/DESIGN.md`). This file owns
//! the clap tree and the exit-code mapping; `client.rs` owns HTTP,
//! `credential.rs` the keychain model, `config.rs` the non-secret settings,
//! and `commands/*` one module per command group.

use std::path::PathBuf;

use alp::client::{self, Alpaca};
use alp::commands::{self, Ctx};
use alp::config::{Config, BIN, REPO};
use alp::credential;
use alp::mcp::{AlpacaTools, LiveSource};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_auth::{LoginArgs, LogoutArgs, SetCredentialArgs};
use pk_cli_config::ConfigStore;
use pk_cli_core::{output, CliError, CommonArgs, RangeArgs};
use pk_cli_http::ApiArgs;
use pk_cli_secrets::CredentialStore;
use pk_cli_selfupdate::{SelfUpdateArgs, Updater};

/// Read-only access to an Alpaca brokerage account (conforms to piekstra-cli/1).
///
/// Official Trading API; the account's own API keys. See docs/api.md.
#[derive(Parser, Debug)]
#[command(name = BIN, version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    common: CommonArgs,

    /// Alpaca environment: `live` (default) or `paper`. Each has its own API
    /// key pair and its own keychain slot.
    #[arg(long, global = true, value_name = "ENV", env = "ALP_ENV")]
    env: Option<String>,

    /// Override the config file location.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Credential management and status.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Non-secret settings (`env`, `base_url`).
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Trading account summary (account/v1).
    Account,
    /// Current holdings.
    Positions {
        #[command(subcommand)]
        cmd: Option<PositionsCmd>,
    },
    /// Account activities — the realized-gains and income raw material.
    #[command(subcommand)]
    Transactions(TransactionsCmd),
    /// Dividend and interest activities — the 1099-DIV/INT raw material.
    #[command(subcommand)]
    Dividends(DividendsCmd),
    /// Raw read-only API passthrough (GET only, Trading API origin only).
    Api(ApiArgs),
    /// Serve balances, positions and activity to Claude and other MCP
    /// clients over stdio.
    Mcp(pk_cli_mcp::McpArgs),
    /// Update to the latest release from GitHub.
    SelfUpdate(SelfUpdateArgs),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
}

#[derive(Subcommand, Debug)]
enum AuthCmd {
    /// Validate an API key pair against the selected environment and store it.
    Login(LoginArgs),
    /// Report credential state (auth-status/v1).
    Status(commands::auth::StatusArgs),
    /// No session exists to clear; --forget removes the stored API keys.
    Logout(LogoutArgs),
    /// Raw keychain write for rotation / headless setup.
    SetCredential(SetCredentialArgs),
}

#[derive(Subcommand, Debug)]
enum PositionsCmd {
    /// List holdings (position-list/v1). The default when no verb is given.
    #[command(visible_alias = "ls")]
    List {
        /// Maximum records to return.
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },
}

#[derive(Subcommand, Debug)]
enum TransactionsCmd {
    /// List account activities in a date range (transaction-list/v1).
    ///
    /// Items are the provider's activity rows VERBATIM — trade activities
    /// (activity_type FILL) and non-trade activities (DIV, INT, FEE, …) share
    /// the feed; branch on activity_type. Defaults to year-to-date.
    #[command(visible_alias = "ls")]
    List {
        #[command(flatten)]
        range: RangeArgs,
        /// Only these activity types (e.g. FILL, DIV, INT — see docs/api.md
        /// §3.3 for the full list). Repeat for several. Default: every type.
        #[arg(long = "type", value_name = "TYPE")]
        types: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum DividendsCmd {
    /// List dividend and interest activities in a date range
    /// (dividend-list/v1) — the DIV*/INT* slice of `transactions list`.
    #[command(visible_alias = "ls")]
    List {
        #[command(flatten)]
        range: RangeArgs,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the resolved config file path.
    Path,
    /// Show the effective configuration.
    Show,
    /// Set a config key (`env` = live|paper, `base_url`).
    Set { key: String, value: String },
    /// Remove a config key.
    Unset { key: String },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        std::process::exit(output::fail(&e, cli.common.json));
    }
}

fn run(cli: &Cli) -> Result<(), CliError> {
    // Commands that need neither config, keychain, nor network are answered
    // first, so `--help`, `completions`, and `info` can never prompt or hang.
    match &cli.command {
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), BIN, &mut std::io::stdout());
            return Ok(());
        }
        Command::Info => return commands::misc::info(cli.common.json),
        Command::SelfUpdate(args) => {
            return Updater {
                repo: REPO.into(),
                binary: BIN.into(),
                target: env!("BUILD_TARGET").into(),
                current: env!("CARGO_PKG_VERSION").into(),
            }
            .run(args, cli.common.json, cli.common.quiet)
        }
        _ => {}
    }

    // Everything checkable from the arguments alone is checked here, so bad
    // input fails identically whether or not the machine has credentials —
    // and never reaches the keychain or the provider (SPEC v1 §1.2).
    validate(&cli.command)?;

    let store = ConfigStore::new(BIN).with_override(cli.config.clone());
    let cfg: Config = store.load()?;
    let env = cfg.env(cli.env.as_deref())?;
    let base_url = cfg.base_url(cli.env.as_deref())?;
    let creds = CredentialStore::for_binary(BIN);

    match &cli.command {
        Command::Config(cmd) => {
            commands::misc::config(&cli.common, &store, cfg, cmd_to_config_op(cmd))
        }
        // The key pair is read on a tool's first call, never at startup:
        // listing the tools touches no credential.
        Command::Mcp(args) => pk_cli_mcp::run(
            args,
            AlpacaTools::new(
                LiveSource::new(env, base_url, creds, cli.common.verbose),
                env,
            ),
        ),
        Command::Auth(cmd) => match cmd {
            AuthCmd::Login(args) => {
                commands::auth::login(&cli.common, env, &base_url, &creds, args)
            }
            AuthCmd::Status(args) => {
                commands::auth::status(&cli.common, env, &base_url, &creds, args.verify)
            }
            AuthCmd::Logout(args) => commands::auth::logout(env, &creds, args.forget),
            AuthCmd::SetCredential(args) => commands::auth::set_credential(env, &creds, args),
        },
        _ => {
            // The `api` origin check runs before the credential is read: an
            // off-origin URL is a usage error, not a reason to open the
            // keychain.
            if let Command::Api(args) = &cli.command {
                let url = args.url(&base_url);
                if !client::same_origin(&base_url, &url) {
                    return Err(CliError::Usage(format!(
                        "`alp api` only talks to {base_url} — refusing {url}"
                    )));
                }
            }

            // Everything below reads the provider.
            let keys = credential::require(&creds, env)?;
            let api = Alpaca::new(base_url, keys, cli.common.verbose)?;
            let ctx = Ctx::new(cli.common.clone(), env, api)?;
            match &cli.command {
                Command::Account => commands::account::show(&ctx),
                Command::Positions { cmd } => {
                    let limit = match cmd {
                        Some(PositionsCmd::List { limit }) => *limit,
                        None => None,
                    };
                    commands::positions::list(&ctx, limit)
                }
                Command::Transactions(TransactionsCmd::List { range, types }) => {
                    commands::activities::list_transactions(&ctx, range, types)
                }
                Command::Dividends(DividendsCmd::List { range }) => {
                    commands::activities::list_dividends(&ctx, range)
                }
                Command::Api(args) => commands::api::run(&ctx, args),
                // Handled above.
                Command::Auth(_)
                | Command::Config(_)
                | Command::Mcp(_)
                | Command::SelfUpdate(_)
                | Command::Completions { .. }
                | Command::Info => unreachable!("dispatched before provider setup"),
            }
        }
    }
}

/// Argument checks that need nothing but the arguments themselves.
fn validate(command: &Command) -> Result<(), CliError> {
    match command {
        Command::Transactions(TransactionsCmd::List { range, types }) => {
            range.validate()?;
            if types.iter().any(|t| t.trim().is_empty()) {
                return Err(CliError::Usage("--type must not be empty".into()));
            }
            Ok(())
        }
        Command::Dividends(DividendsCmd::List { range }) => range.validate(),
        Command::Api(args) => {
            // Read-only is a safety property of this tool, so the refusal is a
            // usage error (exit 2), not a confirmation prompt (exit 6): there
            // is no --force that would make a write work, and reporting 6
            // would invite a caller to retry as though there were.
            let method = args.method.trim().to_uppercase();
            if method != "GET" {
                return Err(CliError::Usage(format!(
                    "`alp api` is read-only and accepts GET only (got {method})"
                )));
            }
            if args.data.is_some() {
                return Err(CliError::Usage(
                    "`alp api` is read-only; a GET carries no --data body".into(),
                ));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Translate the clap enum into the command module's own operation type, so
/// `commands::misc` doesn't depend on the clap tree.
fn cmd_to_config_op(cmd: &ConfigCmd) -> commands::misc::ConfigOp<'_> {
    match cmd {
        ConfigCmd::Path => commands::misc::ConfigOp::Path,
        ConfigCmd::Show => commands::misc::ConfigOp::Show,
        ConfigCmd::Set { key, value } => commands::misc::ConfigOp::Set(key, value),
        ConfigCmd::Unset { key } => commands::misc::ConfigOp::Unset(key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// clap's own debug assertions catch conflicting shorts (e.g. a
    /// subcommand `-q` shadowing the global `--quiet`) only when the command
    /// tree is built, so build it.
    #[test]
    fn command_tree_is_valid() {
        Cli::command().debug_assert();
    }
}
