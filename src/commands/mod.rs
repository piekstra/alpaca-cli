//! One module per command group: the clap-facing half of each read. The
//! provider reads themselves are [`crate::reads`], shared with the MCP tools
//! and the Worker module.

pub mod account;
pub mod activities;
pub mod api;
pub mod auth;
pub mod misc;
pub mod positions;

use pk_cli_core::{CliError, CommonArgs};

use crate::client::NativeAlpaca;
use crate::config::Env;

/// Everything a provider-reading command needs, built only after arguments
/// validate and the credential resolves.
pub struct Ctx {
    pub common: CommonArgs,
    pub env: Env,
    pub api: NativeAlpaca,
}

impl Ctx {
    pub fn new(common: CommonArgs, env: Env, api: NativeAlpaca) -> Result<Self, CliError> {
        Ok(Ctx { common, env, api })
    }
}
