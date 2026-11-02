//! `alp` as a library, so the fixture-contract tests in `tests/` exercise the
//! same mapping code the binary runs — a provider field rename fails a test
//! loudly instead of silently emptying a column. The MCP tools in [`mcp`]
//! are reads over the same client.

pub mod client;
pub mod commands;
pub mod config;
pub mod credential;
pub mod mcp;
