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
tag **v0.7.0** — reuse before writing (dates, output, `RangeArgs`, `Paged`,
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

## Build / test / lint

```console
$ make verify     # fmt-check + clippy -D warnings + tests + smoke — the gate
$ make install    # cargo install + dev-sign (see below)
```

- Tests are **fully offline**: no network, no keychain, ever. SPEC §1.2's
  validate-before-keychain rule is what makes the auth surface testable
  offline — argument failures must stay ahead of any keychain read.
- macOS signing: every Makefile target that produces a binary re-signs it via
  `~/Dev/cli-common/scripts/dev-sign.sh`. Skipping this makes keychain
  "Always Allow" grants silently expire on each rebuild.
- CI mirrors `make verify` on Linux (cost discipline; the keychain runtime
  path is covered by local dogfooding of the signed binary).

## Provider model (the short version — details in docs/api.md)

- Auth: two headers, `APCA-API-KEY-ID` / `APCA-API-SECRET-KEY`. The pair is
  a full-power credential (it can trade); it lives only in the OS keychain,
  service `piekstra.alp`, one entry per environment.
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
the error paths. The owner runs `alp auth login` themselves — never ask for
key material, and never echo it.
