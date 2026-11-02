# alpaca-cli (`alp`)

Read-only CLI for an [Alpaca](https://alpaca.markets) brokerage account, over
the official [Trading API](https://docs.alpaca.markets). Built to feed tax
work: realized-gains raw material (fills), dividend/interest income, current
positions, and the account summary — each as a stable, schema-tagged JSON DTO.

Unofficial; not affiliated with or endorsed by AlpacaDB, Inc. or Alpaca Securities LLC.

Conforms to **piekstra-cli spec v1** (`piekstra-cli/1`; see
[`cli-common`](https://github.com/piekstra/cli-common)).

**Read-only.** There is no order-entry, transfer, or settings code path in
this binary, and `alp api` accepts `GET` only. If a write is ever added, this
claim changes in the same commit.

## Install

```console
$ git clone https://github.com/piekstra/alpaca-cli && cd alpaca-cli
$ make install     # cargo install + macOS dev-signing (keeps keychain grants stable)
```

## Setup

Generate an API key pair on the Alpaca dashboard, then:

```console
$ alp auth login            # prompts for key ID + secret key, verifies, stores in the OS keychain
$ alp auth status           # auth-status/v1; add --verify to spend one request proving the keys work
```

Live is the default environment. Paper trading has its **own key pair** and
its own keychain slot:

```console
$ alp config set env paper  # or per-invocation: alp --env paper …  (env var: ALP_ENV)
$ alp --env paper auth login
```

## Commands

| Command | JSON schema | What it answers |
|---|---|---|
| `alp account` | `account/v1` | Account number, status, cash/equity/buying power (string-decimal `Money`) |
| `alp positions [list] [--limit N]` | `position-list/v1` | Holdings: symbol, quantity, market value, entry/current price, unrealized P/L |
| `alp transactions list [--since D --until D --limit N] [--type T]...` | `transaction-list/v1` | Account activities **verbatim** — fills, dividends, interest, fees; defaults to year-to-date |
| `alp dividends list [--since D --until D --limit N]` | `dividend-list/v1` | The DIV\*/INT\* slice of the same feed — 1099-DIV/INT raw material |
| `alp api GET <PATH>` | (provider JSON) | Raw read-only passthrough, Trading API origin only |
| `alp auth login/status/logout/set-credential` | `auth-status/v1` | Credential lifecycle |
| `alp config path/show/set/unset` | — | `env` (`live`/`paper`), `base_url` |
| `alp info` | `cli-info/v1` | Machine discovery (offline) |
| `alp self-update [--check]` | `self-update/v1` | GitHub-release update |
| `alp completions <shell>` | — | Shell completions |

Every command takes `--json`; exit codes follow the family contract
(0 ok · 2 usage · 3 auth · 4 not found · 5 upstream · 6 confirmation).

### The transactions feed is verbatim

`transaction-list/v1` items are Alpaca's activity rows exactly as sent —
every key, every string decimal. Two shapes share the feed: trade activities
(`activity_type: "FILL"`, dated by `transaction_time`) and non-trade
activities (`DIV`, `DIVNRA`, `INT`, `FEE`, …, dated by `date`, amount in
`net_amount`). **Branch on `activity_type`**, not on which keys exist. This
is order/income history, not a realized-gain report — Alpaca publishes no
cost-basis endpoint, and the consolidated 1099 remains the source of truth
for filed figures (see `docs/api.md`).

```console
$ alp transactions list --since 2026-01-01 --until 2026-12-31 --json   # the tax-year pull
$ alp transactions list --type FILL --json                             # trades only
$ alp dividends list --json                                            # income only
```

## Development

```console
$ make verify   # fmt-check + clippy -D warnings + offline tests + smoke — the gate
```

Tests are fully offline (no network, no keychain). Fixtures are synthetic —
see `tests/fixtures/README.md` for the scrubbing policy, which a test
enforces. `docs/api.md` records the endpoints and their traps.

## License

MIT or Apache-2.0, at your option.
