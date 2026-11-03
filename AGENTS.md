# AGENTS.md — working agreement for `alpaca-cli`

Canonical agent guide for this repo. `CLAUDE.md` is a thin pointer here.

## What this is

`alp` is a **read-only** CLI over Alpaca's **official** Trading API
(`api.alpaca.markets` / `paper-api.alpaca.markets`, documented at
docs.alpaca.markets). It exists to feed tax work:

- **realized-gains raw material** — `transactions list --since --until`
  emits `transaction-list/v1` whose items are the provider's activity rows
  **VERBATIM** (see the contract below);
- **1099-DIV/INT reconciliation** — `dividends list` (the DIV\*/INT\* slice
  of the same feed);
- **point-in-time holdings and balances** — `positions list`
  (`position-list/v1`) and `account` (`account/v1`).

Conforms to `piekstra-cli/1` (see `~/Dev/cli-common/DESIGN.md` and
`conformance.md`). Shared behavior comes from the `pk-cli-*` crates pinned to
tag **v0.15.1** — reuse before writing (dates, output, `RangeArgs`, `Paged`,
keychain, config, self-update).

## The verbatim DTO contract (do not "improve" it)

`transaction-list/v1` and `dividend-list/v1` items are Alpaca's activity
rows byte-for-byte: no renamed keys, no `Money` wrapping, no rounding, no
float ever touching a value. tax-cli stores these DTOs as source documents;
a helpful reshape here is a silent edit to a tax record. The CLI may only
*select* (range/limit/type), *sort* (newest first), and *envelope*
(`Paged`). `tests/fixture_shapes.rs::kept_activity_rows_are_byte_identical_to_the_wire`
enforces this — keep it passing.

`account/v1` and `position-list/v1` are curated family shapes, but their
money amounts are still the wire's strings, untouched.

## Layout and features

- `src/client.rs` — `Alpaca<T>`, the GET-only client over pk-cli-core's
  `Transport`: blocking reqwest natively (`Alpaca::new`, paced at 4 req/s),
  the Worker's `fetch` in the module (`Alpaca::with_transport`).
- `src/reads.rs` — the provider reads (account, positions, the activities
  walk) and their mapping and filtering, clap-free and async; the commands
  drive them with `pk_cli_core::block_on`.
- `src/commands/*` — the clap-facing half of each command (feature
  `native`).
- `src/mcp.rs` — the MCP tools; `src/worker.rs` — the same tools as a
  WebAssembly module (feature `wasm`).

Features: `native` (default) is everything the binary needs (keychain,
clap, self-update, blocking HTTP, the stdio MCP server); `wasm` is the
Worker module. The library builds for `wasm32-unknown-unknown` with
`--no-default-features --features wasm`, so nothing that needs the OS (the
keychain, `SystemTime`, `std::thread`, blocking I/O) may reach `reads`,
`client`'s shared path or `mcp`'s toolset: the toolset takes today's date
from a `Today` function, the Worker's clock in the module.

## MCP tools (`alp mcp`, and the Worker module)

`src/mcp.rs` serves `alpaca_portfolio` and `alpaca_activities` over stdio
(cli-common's `pk-cli-mcp`, DESIGN.md §1.9), reading through the same
client and mapping code the commands use, behind a `Source` seam:
`LiveSource` for `alp mcp`, `ClientSource` for the Worker module.

`src/worker.rs` exports the same toolset as a WebAssembly module for
cli-common's deploy-your-own Cloudflare connector (`pk_cli_mcp::export_wasm!`).
`make wasm` builds it and checks it against the host's contract
(`scripts/check-wasm.mjs`); the release attaches it as
`alp-mcp-<version>.wasm` with its `.sha256`, which the connector pins. Its
settings are the Worker's secrets and vars by the CLI's own names
(`ALP_API_KEY_ID`, `ALP_API_SECRET_KEY`, optional `ALP_ENV` and
`ALP_BASE_URL`; docs/api.md §6), and its instructions and auth errors say
the keys are Worker secrets.

- `tests/mcp_tools.rs` runs every tool in process over the fixtures, plus
  cli-common's conformance check (names, annotations, descriptions, flat
  inputs, output schemas, definitions under 8,000 characters).
- `tests/worker.rs` (feature `wasm`) drives the module's toolset natively
  through pk-cli-mcp's stand-in host: every request's headers and host, the
  activities walk, and the auth errors that name the Worker's secrets.
- `tests/mcp_stdio.rs` drives the real binary over stdio; `tests/packaging.rs`
  holds `packaging/mcpb/manifest.json` (the Claude Desktop extension the
  release workflow packs) to the code: version, tools, env vars.
- **A tool never signs in.** It never prompts, starts a login or creates a
  keychain item; a missing or rejected pair is an `auth` error telling the
  model to have the user run `alp auth login` in a terminal. `tools/list`
  touches no credential: the pair is read on the first call.
- The tools are `ToolDef::read`s. A write would be its own `ToolDef::write`,
  never mixed into a read, and none may move money.
- The pair also comes from `ALP_API_KEY_ID` + `ALP_API_SECRET_KEY`
  (`credential::resolve`: env before keychain; half a pair is exit 3), which
  is how the `.mcpb` passes the values Claude Desktop stores.

## Build / test / lint

```console
$ make verify     # fmt-check + clippy (native and wasm features) + tests + wasm32 clippy + smoke — the gate
$ make wasm       # the Worker module, checked (needs the wasm32-unknown-unknown target and Node)
$ make install    # cargo install + dev-sign (see below)
```

- Tests are **fully offline**: no network, no keychain, ever. SPEC §1.2's
  validate-before-keychain rule is what makes the auth surface testable
  offline — argument failures must stay ahead of any keychain read.
- macOS signing: every Makefile target that produces a binary re-signs it via
  `~/Dev/cli-common/scripts/dev-sign.sh`. Skipping this makes keychain
  "Always Allow" grants silently expire on each rebuild.
- CI mirrors `make verify` on Linux and builds the module with `make wasm`
  (cost discipline; the keychain runtime path is covered by local
  dogfooding of the signed binary).

## Provider model (the short version — details in docs/api.md)

- Auth: two headers, `APCA-API-KEY-ID` / `APCA-API-SECRET-KEY`. The pair is
  a full-power credential (it can trade); it lives only in the OS keychain,
  service `piekstra.alp`, one entry per environment, or arrives per run in
  `ALP_API_KEY_ID` + `ALP_API_SECRET_KEY` (env wins over the keychain).
- `live` and `paper` are **different hosts with different key pairs**; the
  env names the keychain slot, so keys can't cross environments.
- Activities paging: `page_token` = last row's id, newest-first; the client
  ignores the server's date filters and range-filters inclusively itself
  (docs/api.md §3.3 for why).
- The market-data API (`data.alpaca.markets`) is a different host and out of
  scope; `alp api` refuses it by origin.

## Non-negotiables

- **Read-only.** The client type has no write method; `alp api` is GET-only.
  Changing that requires changing README/SECURITY in the same commit.
- **Repo is public-clean.** No real account numbers, balances, key
  fragments, or personal data in source, fixtures, docs, or commit messages.
  Runtime output may carry them; git may not. Offline tests enforce both the
  fixture scrub policy (positively stated) and a personal-data scan over
  `git ls-files`.
- **Fixtures**: synthetic, doc-derived (`tests/fixtures/README.md`). When a
  live capture ever disagrees, fix the fixture *and* record the difference
  in docs/api.md.
- One JSON document per invocation; diagnostics on stderr; exit codes 0/2/3/4/5/6.

## Dogfooding

`make verify` is necessary, not sufficient. After changes, `make install`
and drive the installed binary: `alp info`, `alp auth status`,
`alp account --json`, `alp positions --json`, `alp transactions list --json`,
the error paths. For the MCP tools, drive the installed `alp mcp` over stdio
with JSON-RPC lines (`initialize`, `tools/list`, `tools/call`);
`alp mcp --list-tools` prints the definitions without a credential.

The owner runs `alp auth login` themselves — never ask for key material, and
never echo it.
