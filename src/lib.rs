//! `alp` as a library, so the fixture-contract tests in `tests/` exercise the
//! same mapping code the binary runs — a provider field rename fails a test
//! loudly instead of silently emptying a column.
//!
//! The reads in [`reads`] go through the [`client`], written once against
//! pk-cli-core's `Transport`. The MCP tools in [`mcp`] are those reads,
//! served by `alp mcp` natively and by the WebAssembly module in [`worker`]
//! (feature `wasm`) in a deploy-your-own Cloudflare Worker.
//!
//! Features: `native` (default) is everything the terminal binary needs
//! (keychain, clap, self-update, blocking HTTP, the stdio MCP server); `wasm`
//! is the Worker module. The library builds for `wasm32-unknown-unknown`
//! with `--no-default-features --features wasm`.

#[cfg(all(target_arch = "wasm32", not(feature = "wasm")))]
compile_error!(
    "on wasm32, build with `--no-default-features --features wasm`: the module's clock comes \
     from the Worker through pk-cli-mcp's host"
);

pub mod client;
#[cfg(feature = "native")]
pub mod commands;
pub mod config;
pub mod credential;
pub mod mcp;
pub mod reads;
#[cfg(feature = "wasm")]
pub mod worker;
