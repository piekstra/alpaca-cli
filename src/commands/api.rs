//! `alp api GET <PATH>` — raw read-only passthrough for the Trading API.
//!
//! The method is validated in `main::validate` (GET only, no `--data`) so a
//! write verb fails before config, keychain, or network. The origin check
//! lives in `main` too, ahead of the credential read: the request carries the
//! account's key headers, so an absolute URL is accepted only when it stays
//! on the configured API origin. (The market-data API at
//! `data.alpaca.markets` is a different host and deliberately out of scope —
//! docs/api.md §5.)

use pk_cli_core::{output, CliError};
use pk_cli_http::ApiArgs;

use crate::commands::Ctx;

pub fn run(ctx: &Ctx, args: &ApiArgs) -> Result<(), CliError> {
    let url = args.url(ctx.api.base_url());
    // Defense in depth: main already refused off-origin URLs.
    if !ctx.api.same_origin(&url) {
        return Err(CliError::Usage(format!(
            "`alp api` only talks to {} — refusing {url}",
            ctx.api.base_url()
        )));
    }
    let path = &url[ctx.api.base_url().len()..];
    let v = ctx.api.get(path, &[])?;
    // A passthrough's output *is* the provider's JSON, in both modes.
    output::json(&v);
    Ok(())
}
